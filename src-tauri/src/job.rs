//! Background jobs (index/add/backup/verify) — multiple can run in parallel; each has its own cancel flag.

use crate::disk;
use crate::vault::{self, ControlledFile};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tauri::{AppHandle, Emitter};

pub const EVT_PROGRESS: &str = "controlled-job-progress";
pub const EVT_FINISHED: &str = "controlled-job-finished";

struct JobSlot {
    kind: String,
    cancel: Arc<AtomicBool>,
}

static JOBS: LazyLock<Mutex<HashMap<String, JobSlot>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Serialize)]
pub struct JobStart {
    pub job_id: String,
    pub total: usize,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobProgress {
    pub job_id: String,
    pub phase: String,
    pub current: usize,
    pub total: usize,
    pub rel_path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobFinished {
    pub job_id: String,
    pub kind: String,
    pub ok: bool,
    pub cancelled: bool,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total: usize,
    pub message: String,
    pub files: Vec<ControlledFile>,
}

/// Job id numeric part: local 年月日时分秒 (YYYYMMDDHHmmss), e.g. batch-20260928160700.
fn new_job_id(prefix: &str) -> String {
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    format!("{prefix}-{stamp}")
}

/// Register a new job; returns (job_id, cancel flag). Caller must `finish_job` when done.
pub fn register_job(kind: &str) -> (String, Arc<AtomicBool>) {
    let job_id = new_job_id(kind);
    let cancel = Arc::new(AtomicBool::new(false));
    if let Ok(mut g) = JOBS.lock() {
        g.insert(
            job_id.clone(),
            JobSlot {
                kind: kind.to_string(),
                cancel: cancel.clone(),
            },
        );
    }
    (job_id, cancel)
}

pub fn finish_job(job_id: &str) {
    if let Ok(mut g) = JOBS.lock() {
        g.remove(job_id);
    }
}

/// Cancel one job by id. Returns true if found and marked.
pub fn cancel_job_id(job_id: &str) -> bool {
    if let Ok(g) = JOBS.lock() {
        if let Some(slot) = g.get(job_id) {
            slot.cancel.store(true, Ordering::SeqCst);
            return true;
        }
    }
    false
}

/// Cancel all jobs whose kind matches any of the given prefixes (e.g. "add", "index").
pub fn cancel_kinds(prefixes: &[&str]) -> bool {
    let mut any = false;
    if let Ok(g) = JOBS.lock() {
        for slot in g.values() {
            if prefixes.iter().any(|p| slot.kind == *p || slot.kind.starts_with(p)) {
                slot.cancel.store(true, Ordering::SeqCst);
                any = true;
            }
        }
    }
    any
}

pub fn is_cancelled(flag: &AtomicBool) -> bool {
    flag.load(Ordering::SeqCst)
}


fn emit_progress(app: &AppHandle, p: JobProgress) {
    let _ = app.emit(EVT_PROGRESS, p);
}

fn emit_finished(app: &AppHandle, f: JobFinished) {
    let _ = app.emit(EVT_FINISHED, f);
}

fn run_hash_job(
    app: &AppHandle,
    drive_root: &PathBuf,
    files: Vec<PathBuf>,
    job_id: &str,
    kind: &str,
    cancel: &AtomicBool,
) {
    let existing: std::collections::HashSet<String> = vault::list_controlled_files(drive_root)
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.rel_path.to_lowercase())
        .collect();

    let mut to_hash: Vec<PathBuf> = Vec::new();
    let mut skipped = 0usize;
    for abs in files {
        match vault::normalize_rel_path(drive_root, &abs) {
            Ok(rel) if existing.contains(&rel.to_lowercase()) => {
                skipped += 1;
            }
            _ => to_hash.push(abs),
        }
    }

    let total = to_hash.len();
    let mut added = 0usize;
    let mut failed = 0usize;
    let mut out_files: Vec<ControlledFile> = Vec::new();
    let mut cancelled = false;

    emit_progress(
        app,
        JobProgress {
            job_id: job_id.to_string(),
            phase: "hashing".into(),
            current: 0,
            total,
            rel_path: None,
            message: format!("开始计算哈希：待处理 {total} 个，跳过已受控 {skipped} 个"),
        },
    );

    for (i, abs) in to_hash.iter().enumerate() {
        if is_cancelled(cancel) {
            cancelled = true;
            break;
        }
        let rel_display = vault::normalize_rel_path(drive_root, abs)
            .unwrap_or_else(|_| abs.to_string_lossy().to_string());
        emit_progress(
            app,
            JobProgress {
                job_id: job_id.to_string(),
                phase: "hashing".into(),
                current: i + 1,
                total,
                rel_path: Some(rel_display.clone()),
                message: format!("正在处理 ({}/{total}): {rel_display}", i + 1),
            },
        );

        match vault::upsert_controlled_file(
            drive_root,
            abs,
            crate::hashutil::DEFAULT_SAMPLE_RATIO,
            crate::hashutil::DEFAULT_SAMPLE_CHUNK_MB,
        ) {
            Ok(row) => {
                added += 1;
                out_files.push(row);
            }
            Err(e) => {
                failed += 1;
                emit_progress(
                    app,
                    JobProgress {
                        job_id: job_id.to_string(),
                        phase: "hashing".into(),
                        current: i + 1,
                        total,
                        rel_path: Some(rel_display),
                        message: format!("失败: {e}"),
                    },
                );
            }
        }
    }

    let message = if cancelled {
        format!("已取消：新增 {added}，跳过已受控 {skipped}，失败 {failed}")
    } else if failed == 0 {
        format!("完成：新增 {added} 个受控文件，跳过已受控 {skipped} 个")
    } else {
        format!("完成：新增 {added}，跳过已受控 {skipped}，失败 {failed}，待处理 {total}")
    };

    emit_finished(
        app,
        JobFinished {
            job_id: job_id.to_string(),
            kind: kind.to_string(),
            ok: !cancelled && failed == 0,
            cancelled,
            added,
            skipped,
            failed,
            total,
            message,
            files: out_files,
        },
    );

    finish_job(job_id);
}

/// Expand selected paths (files/dirs) and hash in a background thread.
pub fn start_add_controlled(
    app: AppHandle,
    drive: &str,
    paths: &[String],
) -> Result<JobStart, String> {
    let root = disk::normalize_drive_root(drive)?;
    if !disk::is_backup_disk(&root) {
        return Err("当前盘不是 DataVault 备份盘，请先标记".into());
    }
    let (job_id, cancel) = register_job("add");
    let job_id_ret = job_id.clone();
    let paths = paths.to_vec();

    std::thread::Builder::new()
        .name("datavault-index".into())
        .spawn(move || {
            emit_progress(
                &app,
                JobProgress {
                    job_id: job_id.clone(),
                    phase: "scanning".into(),
                    current: 0,
                    total: 0,
                    rel_path: None,
                    message: "正在收集文件列表…".into(),
                },
            );
            let files = match vault::expand_paths_to_files(&root, &paths) {
                Ok(f) => f,
                Err(e) => {
                    emit_finished(
                        &app,
                        JobFinished {
                            job_id: job_id.clone(),
                            kind: "add".into(),
                            ok: false,
                            cancelled: false,
                            added: 0,
                            skipped: 0,
                            failed: 0,
                            total: 0,
                            message: e,
                            files: vec![],
                        },
                    );
                    finish_job(&job_id);
                    return;
                }
            };
            run_hash_job(&app, &root, files, &job_id, "add", &cancel);
        })
        .map_err(|e| {
            finish_job(&job_id_ret);
            format!("无法启动后台任务: {e}")
        })?;

    Ok(JobStart {
        job_id: job_id_ret,
        total: 0,
        kind: "add".into(),
    })
}

/// Scan entire backup disk (skip `.datavault`) and build index in background.
pub fn start_index_disk(app: AppHandle, drive: &str) -> Result<JobStart, String> {
    let root = disk::normalize_drive_root(drive)?;
    if !disk::is_backup_disk(&root) {
        return Err("当前盘不是 DataVault 备份盘，请先标记".into());
    }
    let (job_id, cancel) = register_job("index");
    let job_id_ret = job_id.clone();

    std::thread::Builder::new()
        .name("datavault-index".into())
        .spawn(move || {
            emit_progress(
                &app,
                JobProgress {
                    job_id: job_id.clone(),
                    phase: "scanning".into(),
                    current: 0,
                    total: 0,
                    rel_path: None,
                    message: format!("正在扫描 {} …", root.display()),
                },
            );
            if is_cancelled(&cancel) {
                emit_finished(
                    &app,
                    JobFinished {
                        job_id: job_id.clone(),
                        kind: "index".into(),
                        ok: false,
                        cancelled: true,
                        added: 0,
                        skipped: 0,
                        failed: 0,
                        total: 0,
                        message: "已取消".into(),
                        files: vec![],
                    },
                );
                finish_job(&job_id);
                return;
            }
            let mut files = Vec::new();
            if let Err(e) = vault::collect_files_under(&root, &mut files) {
                emit_finished(
                    &app,
                    JobFinished {
                        job_id: job_id.clone(),
                        kind: "index".into(),
                        ok: false,
                        cancelled: false,
                        added: 0,
                        skipped: 0,
                        failed: 0,
                        total: 0,
                        message: e,
                        files: vec![],
                    },
                );
                finish_job(&job_id);
                return;
            }
            run_hash_job(&app, &root, files, &job_id, "index", &cancel);
        })
        .map_err(|e| {
            finish_job(&job_id_ret);
            format!("无法启动后台任务: {e}")
        })?;

    Ok(JobStart {
        job_id: job_id_ret,
        total: 0,
        kind: "index".into(),
    })
}

/// Legacy: cancel all add/index jobs.
pub fn cancel_job() -> bool {
    cancel_kinds(&["add", "index"])
}

pub fn is_running() -> bool {
    if let Ok(g) = JOBS.lock() {
        g.values().any(|s| s.kind == "add" || s.kind == "index")
    } else {
        false
    }
}
