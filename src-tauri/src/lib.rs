mod disk;
mod hashutil;
mod job;
mod vault;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use tauri::{Emitter, Manager};

use disk::DiskJson;
use hashutil::{compute_fast_md5, compute_md5_full};
use job::JobStart;
use vault::{ControlledFile, ControlledVerifyReport};

const SAMPLE_WINDOW: u64 = 64 * 1024;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DriveInfo {
    pub path: String,
    pub is_backup_disk: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DirEntryInfo {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub is_backup_disk: bool,
    /// File: recorded in vault.db. Dir: at least one controlled file under it.
    pub is_controlled: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileMeta {
    pub rel_path: String,
    pub src_path: String,
    pub dest_path: String,
    pub size: u64,
    pub md5_full: Option<String>,
    pub md5_quick: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BackupBatch {
    pub id: String,
    pub created_at: String,
    pub sources: Vec<String>,
    pub destination_root: String,
    pub files: Vec<FileMeta>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyItem {
    pub rel_path: String,
    pub src_path: String,
    pub dest_path: String,
    pub src_hash: Option<String>,
    pub dest_hash: Option<String>,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyReport {
    pub batch_id: String,
    pub mode: String,
    pub items: Vec<VerifyItem>,
    pub passed: usize,
    pub failed: usize,
}

fn metadata_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法获取应用数据目录: {e}"))?;
    let root = dir.join("metadata").join("batches");
    fs::create_dir_all(&root).map_err(|e| format!("创建元数据目录失败: {e}"))?;
    Ok(root)
}

/// Backup batch id: local wall-clock 年月日时分秒, e.g. 20260928160700.
fn now_id() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn read_window(file: &mut File, offset: u64, len: u64) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("seek 失败: {e}"))?;
    let mut buf = vec![0u8; len as usize];
    let mut read_total = 0usize;
    while read_total < buf.len() {
        match file.read(&mut buf[read_total..]) {
            Ok(0) => break,
            Ok(n) => read_total += n,
            Err(e) => return Err(format!("读取失败: {e}")),
        }
    }
    buf.truncate(read_total);
    Ok(buf)
}

/// Legacy quick MD5 for batch backup verify (head/tail 64KiB + size).
fn compute_md5_quick_legacy(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|e| format!("元数据失败 {}: {e}", path.display()))?;
    let size = meta.len();
    let mut file = File::open(path).map_err(|e| format!("打开失败 {}: {e}", path.display()))?;
    let mut hasher = Md5::new();

    if size == 0 {
        // empty
    } else if size <= SAMPLE_WINDOW {
        let data = read_window(&mut file, 0, size)?;
        hasher.update(&data);
    } else if size <= SAMPLE_WINDOW * 2 {
        let head = read_window(&mut file, 0, SAMPLE_WINDOW)?;
        let tail = read_window(&mut file, SAMPLE_WINDOW, size - SAMPLE_WINDOW)?;
        hasher.update(&head);
        hasher.update(&tail);
    } else {
        let head = read_window(&mut file, 0, SAMPLE_WINDOW)?;
        let tail = read_window(&mut file, size - SAMPLE_WINDOW, SAMPLE_WINDOW)?;
        hasher.update(&head);
        hasher.update(&tail);
    }

    hasher.update(size.to_string().as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_files(src: &Path, base: &Path, out: &mut Vec<(PathBuf, PathBuf)>) -> Result<(), String> {
    if src.is_file() {
        let rel = src
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("file"));
        out.push((src.to_path_buf(), rel));
        return Ok(());
    }
    if !src.is_dir() {
        return Err(format!("路径不是文件或目录: {}", src.display()));
    }
    let entries = fs::read_dir(src).map_err(|e| format!("读取目录失败 {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("目录项错误: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, base, out)?;
        } else if path.is_file() {
            let rel = path.strip_prefix(base).unwrap_or(&path).to_path_buf();
            out.push((path, rel));
        }
    }
    Ok(())
}

#[tauri::command]
fn list_drives() -> Result<Vec<DriveInfo>, String> {
    let mut drives = Vec::new();
    for letter in b'A'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        let p = Path::new(&root);
        if p.exists() {
            drives.push(DriveInfo {
                path: root.clone(),
                is_backup_disk: disk::is_backup_disk(p),
            });
        }
    }
    Ok(drives)
}

#[tauri::command]
fn detect_backup_disks() -> Result<Vec<DriveInfo>, String> {
    Ok(list_drives()?
        .into_iter()
        .filter(|d| d.is_backup_disk)
        .collect())
}

#[tauri::command]
fn mark_backup_disk(drive: String) -> Result<DiskJson, String> {
    // Only drive roots (e.g. E:\). Refuse subdirectory paths explicitly.
    let trimmed = drive.trim().trim_end_matches(['\\', '/']);
    let is_root = {
        let chars: Vec<char> = trimmed.chars().collect();
        chars.len() == 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic()
    };
    if !is_root {
        return Err("只能标记盘符根目录为备份盘（例如 E:\\），不能标记子目录".into());
    }
    let root = disk::normalize_drive_root(&drive)?;
    disk::mark_backup_disk(&root)
}

#[tauri::command]
fn add_controlled_files(drive: String, paths: Vec<String>) -> Result<Vec<ControlledFile>, String> {
    // Sync fallback (small sets). Prefer start_add_controlled_files for UI.
    if paths.is_empty() {
        return Err("未选择任何文件".into());
    }
    let root = disk::normalize_drive_root(&drive)?;
    vault::add_controlled_files(&root, &paths)
}

#[tauri::command]
fn start_add_controlled_files(
    app: tauri::AppHandle,
    drive: String,
    paths: Vec<String>,
) -> Result<JobStart, String> {
    if paths.is_empty() {
        return Err("未选择任何文件".into());
    }
    job::start_add_controlled(app, &drive, &paths)
}

#[tauri::command]
fn start_index_backup_disk(app: tauri::AppHandle, drive: String) -> Result<JobStart, String> {
    job::start_index_disk(app, &drive)
}

#[tauri::command]
fn cancel_controlled_job() -> Result<bool, String> {
    Ok(job::cancel_job())
}

#[tauri::command]
fn cancel_job(job_id: String) -> Result<bool, String> {
    Ok(job::cancel_job_id(&job_id))
}

#[tauri::command]
fn controlled_job_running() -> Result<bool, String> {
    Ok(job::is_running())
}


#[tauri::command]
fn resolve_controlled_selection(
    drive: String,
    paths: Vec<String>,
) -> Result<Vec<ControlledFile>, String> {
    let root = disk::normalize_drive_root(&drive)?;
    vault::resolve_controlled_selection(&root, &paths)
}

#[tauri::command]
fn list_controlled_files(drive: String) -> Result<Vec<ControlledFile>, String> {
    let root = disk::normalize_drive_root(&drive)?;
    vault::list_controlled_files(&root)
}

#[tauri::command]
fn verify_controlled_full(
    drive: String,
    rel_paths: Option<Vec<String>>,
) -> Result<ControlledVerifyReport, String> {
    let root = disk::normalize_drive_root(&drive)?;
    vault::verify_controlled(&root, "full", rel_paths)
}

#[tauri::command]
fn verify_controlled_quick(
    drive: String,
    rel_paths: Option<Vec<String>>,
) -> Result<ControlledVerifyReport, String> {
    let root = disk::normalize_drive_root(&drive)?;
    vault::verify_controlled(&root, "quick", rel_paths)
}

#[tauri::command]
fn list_dir(path: String) -> Result<Vec<DirEntryInfo>, String> {
    if path.is_empty() {
        return list_drives().map(|ds| {
            ds.into_iter()
                .map(|d| DirEntryInfo {
                    name: d.path.clone(),
                    path: d.path,
                    is_dir: true,
                    size: 0,
                    is_backup_disk: d.is_backup_disk,
                    is_controlled: false,
                })
                .collect()
        });
    }
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("路径不存在: {path}"));
    }
    if !p.is_dir() {
        return Err(format!("不是目录: {path}"));
    }

    // Controlled-status overlay when browsing a backup disk
    let drive_root = disk::drive_root_of(&p);
    let controlled_rels: Vec<String> = if let Some(ref root) = drive_root {
        if disk::is_backup_disk(root) {
            vault::list_controlled_files(root)
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.rel_path)
                .collect()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let controlled_lower: Vec<String> = controlled_rels
        .iter()
        .map(|s| s.to_lowercase())
        .collect();

    let mut items = Vec::new();
    let entries = fs::read_dir(&p).map_err(|e| format!("无法读取目录（可能无权限）: {e}"))?;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ep = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case(disk::META_DIR) {
            continue;
        }
        let is_dir = ep.is_dir();
        let size = if is_dir {
            0
        } else {
            fs::metadata(&ep).map(|m| m.len()).unwrap_or(0)
        };

        let is_controlled = if controlled_lower.is_empty() {
            false
        } else if let Some(ref root) = drive_root {
            match vault::normalize_rel_path(root, &ep) {
                Ok(rel) => {
                    let rel_l = rel.to_lowercase();
                    if is_dir {
                        let prefix = format!("{rel_l}\\");
                        controlled_lower.iter().any(|c| c.starts_with(&prefix))
                    } else {
                        controlled_lower.iter().any(|c| c == &rel_l)
                    }
                }
                Err(_) => false,
            }
        } else {
            false
        };

        items.push(DirEntryInfo {
            name,
            path: ep.to_string_lossy().to_string(),
            is_dir,
            size,
            is_backup_disk: false,
            is_controlled,
        });
    }
    items.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(items)
}

#[tauri::command]
fn md5_full(path: String) -> Result<String, String> {
    compute_md5_full(Path::new(&path))
}

#[tauri::command]
fn md5_quick(path: String) -> Result<String, String> {
    // Legacy head/tail for ad-hoc; FastMD5 is used for controlled files.
    compute_md5_quick_legacy(Path::new(&path))
}

#[tauri::command]
fn md5_fast(path: String, sample_ratio: Option<f64>, sample_chunk_mb: Option<i64>) -> Result<String, String> {
    compute_fast_md5(
        Path::new(&path),
        sample_ratio.unwrap_or(hashutil::DEFAULT_SAMPLE_RATIO),
        sample_chunk_mb.unwrap_or(hashutil::DEFAULT_SAMPLE_CHUNK_MB),
    )
}

#[tauri::command]
fn backup_paths(
    app: tauri::AppHandle,
    sources: Vec<String>,
    dest: String,
) -> Result<BackupBatch, String> {
    if sources.is_empty() {
        return Err("未选择任何源路径".into());
    }
    if dest.trim().is_empty() {
        return Err("目标目录为空".into());
    }
    let dest_root = PathBuf::from(&dest);
    fs::create_dir_all(&dest_root).map_err(|e| format!("创建目标目录失败: {e}"))?;

    let batch_id = now_id();
    let mut files_meta: Vec<FileMeta> = Vec::new();

    for src_str in &sources {
        let src = PathBuf::from(src_str);
        if !src.exists() {
            files_meta.push(FileMeta {
                rel_path: src_str.clone(),
                src_path: src_str.clone(),
                dest_path: String::new(),
                size: 0,
                md5_full: None,
                md5_quick: None,
                error: Some("源路径不存在".into()),
            });
            continue;
        }

        let mut collected: Vec<(PathBuf, PathBuf)> = Vec::new();
        if src.is_file() {
            let name = src
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("file"));
            collected.push((src.clone(), name));
        } else {
            let top = src
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("folder"));
            let mut inner: Vec<(PathBuf, PathBuf)> = Vec::new();
            collect_files(&src, &src, &mut inner)?;
            for (abs, rel) in inner {
                collected.push((abs, top.join(rel)));
            }
        }

        for (abs, rel) in collected {
            let dest_path = dest_root.join(&rel);
            if let Some(parent) = dest_path.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    files_meta.push(FileMeta {
                        rel_path: rel.to_string_lossy().to_string(),
                        src_path: abs.to_string_lossy().to_string(),
                        dest_path: dest_path.to_string_lossy().to_string(),
                        size: 0,
                        md5_full: None,
                        md5_quick: None,
                        error: Some(format!("创建父目录失败: {e}")),
                    });
                    continue;
                }
            }
            match fs::copy(&abs, &dest_path) {
                Ok(_) => {
                    let size = fs::metadata(&dest_path).map(|m| m.len()).unwrap_or(0);
                    let quick = compute_md5_quick_legacy(&abs).ok();
                    files_meta.push(FileMeta {
                        rel_path: rel.to_string_lossy().to_string(),
                        src_path: abs.to_string_lossy().to_string(),
                        dest_path: dest_path.to_string_lossy().to_string(),
                        size,
                        md5_full: None,
                        md5_quick: quick,
                        error: None,
                    });
                }
                Err(e) => {
                    files_meta.push(FileMeta {
                        rel_path: rel.to_string_lossy().to_string(),
                        src_path: abs.to_string_lossy().to_string(),
                        dest_path: dest_path.to_string_lossy().to_string(),
                        size: 0,
                        md5_full: None,
                        md5_quick: None,
                        error: Some(format!("复制失败: {e}")),
                    });
                }
            }
        }
    }

    let batch = BackupBatch {
        id: batch_id.clone(),
        created_at: now_stamp(),
        sources: sources.clone(),
        destination_root: dest.clone(),
        files: files_meta,
    };

    let meta_dir = metadata_root(&app)?;
    let meta_path = meta_dir.join(format!("{batch_id}.json"));
    let json = serde_json::to_string_pretty(&batch).map_err(|e| format!("序列化失败: {e}"))?;
    fs::write(&meta_path, &json).map_err(|e| format!("写元数据失败: {e}"))?;

    let side = dest_root.join(".datavault");
    let _ = fs::create_dir_all(&side);
    let _ = fs::write(side.join(format!("{batch_id}.json")), &json);

    Ok(batch)
}

#[tauri::command]
fn load_batch(app: tauri::AppHandle, batch_id: String) -> Result<BackupBatch, String> {
    let path = metadata_root(&app)?.join(format!("{batch_id}.json"));
    if !path.exists() {
        return Err(format!("批次不存在: {batch_id}"));
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("读取批次失败: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("解析批次失败: {e}"))
}

#[tauri::command]
fn list_batches(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let dir = metadata_root(&app)?;
    let mut ids = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".json") {
                ids.push(id.to_string());
            }
        }
    }
    ids.sort();
    ids.reverse();
    Ok(ids)
}

#[tauri::command]
fn verify_backup(
    app: tauri::AppHandle,
    batch_id: String,
    mode: String,
) -> Result<VerifyReport, String> {
    let batch = load_batch(app, batch_id.clone())?;
    let use_full = mode == "full" || mode == "完整" || mode == "完整校验";
    let mut items = Vec::new();
    let mut passed = 0usize;
    let mut failed = 0usize;

    for f in &batch.files {
        if f.error.is_some() {
            items.push(VerifyItem {
                rel_path: f.rel_path.clone(),
                src_path: f.src_path.clone(),
                dest_path: f.dest_path.clone(),
                src_hash: None,
                dest_hash: None,
                ok: false,
                message: format!("备份时失败: {}", f.error.clone().unwrap_or_default()),
            });
            failed += 1;
            continue;
        }
        let src = Path::new(&f.src_path);
        let dest = Path::new(&f.dest_path);
        if !src.exists() || !dest.exists() {
            items.push(VerifyItem {
                rel_path: f.rel_path.clone(),
                src_path: f.src_path.clone(),
                dest_path: f.dest_path.clone(),
                src_hash: None,
                dest_hash: None,
                ok: false,
                message: "源或目标文件缺失".into(),
            });
            failed += 1;
            continue;
        }
        let (src_hash, dest_hash) = if use_full {
            (compute_md5_full(src), compute_md5_full(dest))
        } else {
            (
                compute_md5_quick_legacy(src),
                compute_md5_quick_legacy(dest),
            )
        };
        match (src_hash, dest_hash) {
            (Ok(sh), Ok(dh)) => {
                let ok = sh == dh;
                if ok {
                    passed += 1;
                } else {
                    failed += 1;
                }
                items.push(VerifyItem {
                    rel_path: f.rel_path.clone(),
                    src_path: f.src_path.clone(),
                    dest_path: f.dest_path.clone(),
                    src_hash: Some(sh),
                    dest_hash: Some(dh),
                    ok,
                    message: if ok {
                        "一致".into()
                    } else {
                        "不一致".into()
                    },
                });
            }
            (Err(e), _) | (_, Err(e)) => {
                failed += 1;
                items.push(VerifyItem {
                    rel_path: f.rel_path.clone(),
                    src_path: f.src_path.clone(),
                    dest_path: f.dest_path.clone(),
                    src_hash: None,
                    dest_hash: None,
                    ok: false,
                    message: e,
                });
            }
        }
    }

    Ok(VerifyReport {
        batch_id,
        mode: if use_full {
            "full".into()
        } else {
            "quick".into()
        },
        items,
        passed,
        failed,
    })
}



#[derive(Debug, Clone, Serialize)]
pub struct BackupJobFinished {
    pub job_id: String,
    pub kind: String,
    pub ok: bool,
    pub cancelled: bool,
    pub copied: usize,
    pub failed: usize,
    pub total: usize,
    pub message: String,
    pub batch: Option<BackupBatch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyJobFinished {
    pub job_id: String,
    pub kind: String,
    pub ok: bool,
    pub cancelled: bool,
    pub message: String,
    pub controlled: Option<ControlledVerifyReport>,
    pub batch: Option<VerifyReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenericProgress {
    pub job_id: String,
    pub phase: String,
    pub current: usize,
    pub total: usize,
    pub rel_path: Option<String>,
    pub message: String,
}

#[tauri::command]
fn cancel_backup_job() -> Result<bool, String> {
    Ok(job::cancel_kinds(&["backup"]))
}

#[tauri::command]
fn cancel_verify_job() -> Result<bool, String> {
    Ok(job::cancel_kinds(&["controlled-full", "controlled-quick", "batch", "verify"]))
}

#[tauri::command]
fn start_backup(
    app: tauri::AppHandle,
    sources: Vec<String>,
    dest: String,
) -> Result<JobStart, String> {
    if sources.is_empty() {
        return Err("未选择任何源路径".into());
    }
    if dest.trim().is_empty() {
        return Err("目标目录为空".into());
    }
    let (job_id, cancel) = job::register_job("backup");
    let job_id_ret = job_id.clone();
    std::thread::Builder::new()
        .name("datavault-backup".into())
        .spawn(move || {
            let _ = app.emit(
                "backup-job-progress",
                GenericProgress {
                    job_id: job_id.clone(),
                    phase: "copying".into(),
                    current: 0,
                    total: 0,
                    rel_path: None,
                    message: "开始备份…".into(),
                },
            );
            if job::is_cancelled(&cancel) {
                let _ = app.emit(
                    "backup-job-finished",
                    BackupJobFinished {
                        job_id: job_id.clone(),
                        kind: "backup".into(),
                        ok: false,
                        cancelled: true,
                        copied: 0,
                        failed: 0,
                        total: 0,
                        message: "已取消".into(),
                        batch: None,
                    },
                );
                job::finish_job(&job_id);
                return;
            }
            let result = backup_paths(app.clone(), sources, dest);
            match result {
                Ok(batch) => {
                    let cancelled = job::is_cancelled(&cancel);
                    let copied = batch.files.iter().filter(|f| f.error.is_none()).count();
                    let failed = batch.files.iter().filter(|f| f.error.is_some()).count();
                    let total = batch.files.len();
                    let message = if cancelled {
                        format!("备份已取消：成功 {copied}，失败 {failed}，共 {total}")
                    } else {
                        format!("备份完成：成功 {copied}，失败 {failed}，共 {total}")
                    };
                    let _ = app.emit(
                        "backup-job-finished",
                        BackupJobFinished {
                            job_id: job_id.clone(),
                            kind: "backup".into(),
                            ok: !cancelled && failed == 0,
                            cancelled,
                            copied,
                            failed,
                            total,
                            message,
                            batch: Some(batch),
                        },
                    );
                }
                Err(e) => {
                    let _ = app.emit(
                        "backup-job-finished",
                        BackupJobFinished {
                            job_id: job_id.clone(),
                            kind: "backup".into(),
                            ok: false,
                            cancelled: job::is_cancelled(&cancel),
                            copied: 0,
                            failed: 0,
                            total: 0,
                            message: e,
                            batch: None,
                        },
                    );
                }
            }
            job::finish_job(&job_id);
        })
        .map_err(|e| {
            job::finish_job(&job_id_ret);
            format!("无法启动备份任务: {e}")
        })?;
    Ok(JobStart {
        job_id: job_id_ret,
        total: 0,
        kind: "backup".into(),
    })
}

#[tauri::command]
fn start_verify_controlled(
    app: tauri::AppHandle,
    drive: String,
    mode: String,
    rel_paths: Option<Vec<String>>,
    paths: Option<Vec<String>>,
) -> Result<JobStart, String> {
    let kind = if mode == "full" {
        "controlled-full"
    } else {
        "controlled-quick"
    };
    let (job_id, cancel) = job::register_job(kind);
    let job_id_ret = job_id.clone();
    let kind_s = kind.to_string();
    std::thread::Builder::new()
        .name("datavault-verify".into())
        .spawn(move || {
            let _ = app.emit(
                "verify-job-progress",
                GenericProgress {
                    job_id: job_id.clone(),
                    phase: "verifying".into(),
                    current: 0,
                    total: 0,
                    rel_path: None,
                    message: "正在校验受控文件…".into(),
                },
            );
            if job::is_cancelled(&cancel) {
                let _ = app.emit(
                    "verify-job-finished",
                    VerifyJobFinished {
                        job_id: job_id.clone(),
                        kind: kind_s.clone(),
                        ok: false,
                        cancelled: true,
                        message: "已取消".into(),
                        controlled: None,
                        batch: None,
                    },
                );
                job::finish_job(&job_id);
                return;
            }
            let filter = if let Some(ps) = paths {
                if ps.is_empty() {
                    rel_paths
                } else {
                    match disk::normalize_drive_root(&drive)
                        .and_then(|root| vault::resolve_controlled_selection(&root, &ps))
                    {
                        Ok(files) => Some(files.into_iter().map(|f| f.rel_path).collect()),
                        Err(e) => {
                            let _ = app.emit(
                                "verify-job-finished",
                                VerifyJobFinished {
                                    job_id: job_id.clone(),
                                    kind: kind_s.clone(),
                                    ok: false,
                                    cancelled: false,
                                    message: e,
                                    controlled: None,
                                    batch: None,
                                },
                            );
                            job::finish_job(&job_id);
                            return;
                        }
                    }
                }
            } else {
                rel_paths
            };
            let result = if mode == "full" {
                verify_controlled_full(drive, filter)
            } else {
                verify_controlled_quick(drive, filter)
            };
            match result {
                Ok(report) => {
                    let cancelled = job::is_cancelled(&cancel);
                    let msg = if cancelled {
                        format!(
                            "受控校验已取消：通过 {}，失败 {}，缺失 {}，错误 {}",
                            report.passed, report.failed, report.missing, report.errors
                        )
                    } else {
                        format!(
                            "受控校验完成：通过 {}，失败 {}，缺失 {}，错误 {}",
                            report.passed, report.failed, report.missing, report.errors
                        )
                    };
                    let ok = !cancelled
                        && report.failed == 0
                        && report.missing == 0
                        && report.errors == 0;
                    let _ = app.emit(
                        "verify-job-finished",
                        VerifyJobFinished {
                            job_id: job_id.clone(),
                            kind: kind_s,
                            ok,
                            cancelled,
                            message: msg,
                            controlled: Some(report),
                            batch: None,
                        },
                    );
                }
                Err(e) => {
                    let _ = app.emit(
                        "verify-job-finished",
                        VerifyJobFinished {
                            job_id: job_id.clone(),
                            kind: kind_s,
                            ok: false,
                            cancelled: job::is_cancelled(&cancel),
                            message: e,
                            controlled: None,
                            batch: None,
                        },
                    );
                }
            }
            job::finish_job(&job_id);
        })
        .map_err(|e| {
            job::finish_job(&job_id_ret);
            format!("无法启动校验任务: {e}")
        })?;
    // Note: finish_job called inside thread; if Ok path moved job_id, we need finish after emit.
    // Fix: always finish with job_id_ret clone inside — patch below carefully.
    Ok(JobStart {
        job_id: job_id_ret,
        total: 0,
        kind: kind.to_string(),
    })
}

#[tauri::command]
fn start_verify_backup(
    app: tauri::AppHandle,
    batch_id: String,
    mode: String,
) -> Result<JobStart, String> {
    let (job_id, cancel) = job::register_job("batch");
    let job_id_ret = job_id.clone();
    std::thread::Builder::new()
        .name("datavault-verify-batch".into())
        .spawn(move || {
            let _ = app.emit(
                "verify-job-progress",
                GenericProgress {
                    job_id: job_id.clone(),
                    phase: "verifying".into(),
                    current: 0,
                    total: 0,
                    rel_path: None,
                    message: "正在校验备份批次…".into(),
                },
            );
            if job::is_cancelled(&cancel) {
                let _ = app.emit(
                    "verify-job-finished",
                    VerifyJobFinished {
                        job_id: job_id.clone(),
                        kind: "batch".into(),
                        ok: false,
                        cancelled: true,
                        message: "已取消".into(),
                        controlled: None,
                        batch: None,
                    },
                );
                job::finish_job(&job_id);
                return;
            }
            match verify_backup(app.clone(), batch_id, mode) {
                Ok(report) => {
                    let cancelled = job::is_cancelled(&cancel);
                    let msg = if cancelled {
                        format!(
                            "批次校验已取消：通过 {}，失败 {}",
                            report.passed, report.failed
                        )
                    } else {
                        format!(
                            "批次校验完成：通过 {}，失败 {}",
                            report.passed, report.failed
                        )
                    };
                    let ok = !cancelled && report.failed == 0;
                    let _ = app.emit(
                        "verify-job-finished",
                        VerifyJobFinished {
                            job_id: job_id.clone(),
                            kind: "batch".into(),
                            ok,
                            cancelled,
                            message: msg,
                            controlled: None,
                            batch: Some(report),
                        },
                    );
                }
                Err(e) => {
                    let _ = app.emit(
                        "verify-job-finished",
                        VerifyJobFinished {
                            job_id: job_id.clone(),
                            kind: "batch".into(),
                            ok: false,
                            cancelled: job::is_cancelled(&cancel),
                            message: e,
                            controlled: None,
                            batch: None,
                        },
                    );
                }
            }
            job::finish_job(&job_id);
        })
        .map_err(|e| {
            job::finish_job(&job_id_ret);
            format!("无法启动批次校验: {e}")
        })?;
    Ok(JobStart {
        job_id: job_id_ret,
        total: 0,
        kind: "batch".into(),
    })
}


#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_drives,
            detect_backup_disks,
            mark_backup_disk,
            add_controlled_files,
            start_add_controlled_files,
            start_index_backup_disk,
            cancel_controlled_job,
            cancel_job,
            controlled_job_running,
            list_controlled_files,
            resolve_controlled_selection,
            verify_controlled_full,
            verify_controlled_quick,
            list_dir,
            md5_full,
            md5_quick,
            md5_fast,
            backup_paths,
            start_backup,
            cancel_backup_job,
            load_batch,
            list_batches,
            verify_backup,
            start_verify_controlled,
            start_verify_backup,
            cancel_verify_job
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
