//! Background indexing jobs for controlled files (avoids UI freeze).

use crate::disk;
use crate::vault::{self, ControlledFile};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

static CANCEL: AtomicBool = AtomicBool::new(false);
static JOB_RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_JOB: Mutex<Option<String>> = Mutex::new(None);

pub const EVT_PROGRESS: &str = "controlled-job-progress";
pub const EVT_FINISHED: &str = "controlled-job-finished";

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
    pub failed: usize,
    pub total: usize,
    pub message: String,
    pub files: Vec<ControlledFile>,
}

fn new_job_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("job-{nanos}")
}

pub fn cancel_job() -> bool {
    if JOB_RUNNING.load(Ordering::SeqCst) {
        CANCEL.store(true, Ordering::SeqCst);
        true
    } else {
        false
    }
}

pub fn is_running() -> bool {
    JOB_RUNNING.load(Ordering::SeqCst)
}

fn emit_progress(app: &AppHandle, p: JobProgress) {
    let _ = app.emit(EVT_PROGRESS, p);
}

fn emit_finished(app: &AppHandle, f: JobFinished) {
    let _ = app.emit(EVT_FINISHED, f);
}

fn try_claim_job() -> Result<(), String> {
    if JOB_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("已有索引/登记任务在进行中，请先等待或取消".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    Ok(())
}

fn finish_claim() {
    JOB_RUNNING.store(false, Ordering::SeqCst);
    CANCEL.store(false, Ordering::SeqCst);
}

fn run_hash_job(
    app: &AppHandle,
    drive_root: &PathBuf,
    files: Vec<PathBuf>,
    job_id: &str,
    kind: &str,
) {
    let total = files.len();
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
            message: format!("开始计算哈希，共 {total} 个文件"),
        },
    );

    for (i, abs) in files.iter().enumerate() {
        if CANCEL.load(Ordering::SeqCst) {
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
        format!("已取消：成功 {added}，失败 {failed}，共 {total}")
    } else if failed == 0 {
        format!("完成：已登记 {added} 个受控文件")
    } else {
        format!("完成：成功 {added}，失败 {failed}，共 {total}")
    };

    emit_finished(
        app,
        JobFinished {
            job_id: job_id.to_string(),
            kind: kind.to_string(),
            ok: !cancelled && failed == 0,
            cancelled,
            added,
            failed,
            total,
            message,
            files: out_files,
        },
    );

    finish_claim();
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
    try_claim_job()?;

    let job_id = new_job_id();
    if let Ok(mut g) = LAST_JOB.lock() {
        *g = Some(job_id.clone());
    }
    let paths = paths.to_vec();
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
                            failed: 0,
                            total: 0,
                            message: e,
                            files: vec![],
                        },
                    );
                    finish_claim();
                    return;
                }
            };
            run_hash_job(&app, &root, files, &job_id, "add");
        })
        .map_err(|e| {
            finish_claim();
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
    try_claim_job()?;

    let job_id = new_job_id();
    if let Ok(mut g) = LAST_JOB.lock() {
        *g = Some(job_id.clone());
    }
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
            if CANCEL.load(Ordering::SeqCst) {
                emit_finished(
                    &app,
                    JobFinished {
                        job_id: job_id.clone(),
                        kind: "index".into(),
                        ok: false,
                        cancelled: true,
                        added: 0,
                        failed: 0,
                        total: 0,
                        message: "已取消".into(),
                        files: vec![],
                    },
                );
                finish_claim();
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
                        failed: 0,
                        total: 0,
                        message: e,
                        files: vec![],
                    },
                );
                finish_claim();
                return;
            }
            run_hash_job(&app, &root, files, &job_id, "index");
        })
        .map_err(|e| {
            finish_claim();
            format!("无法启动后台任务: {e}")
        })?;

    Ok(JobStart {
        job_id: job_id_ret,
        total: 0,
        kind: "index".into(),
    })
}