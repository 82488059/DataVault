mod disk;
mod hashutil;
mod job;
mod vault;
mod tarutil;
mod metabackup;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::{Emitter, Manager};

use disk::DiskJson;
use hashutil::{compute_fast_md5, compute_md5_full, compute_md5_pair};
use job::JobStart;
use vault::{ControlledFile, ControlledVerifyReport};
use tarutil::TarEntryInfo;
use metabackup::{ExportResult, RestoreResult, VerifyReport as MetaVerifyReport};

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
    /// For controlled dirs: filled asynchronously (受控文件数).
    pub controlled_count: Option<u64>,
    /// For controlled dirs: filled asynchronously (总文件数 under dir).
    pub total_files: Option<u64>,
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
    /// When true, files were packed into one .tar at destination.
    #[serde(default)]
    pub pack_as_tar: bool,
    /// Optional filename regex applied during backup (match or exclude).
    #[serde(default)]
    pub name_regex: Option<String>,
    /// true = exclude matching filenames; false = include only matches.
    #[serde(default)]
    pub regex_exclude: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyItem {
    pub rel_path: String,
    pub src_path: String,
    pub dest_path: String,
    pub src_hash: Option<String>,
    pub dest_hash: Option<String>,
    pub ok: bool,
    /// pass | fail | missing | error — same four statuses as controlled verify
    pub status: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyReport {
    pub batch_id: String,
    pub mode: String,
    pub items: Vec<VerifyItem>,
    pub passed: usize,
    pub failed: usize,
    pub missing: usize,
    pub errors: usize,
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


/// Sanitize user batch name for use as id / filename.
fn sanitize_batch_id(s: &str) -> Result<String, String> {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned
        .trim_matches(|c: char| c == '.' || c == ' ' || c == '_')
        .to_string();
    if cleaned.is_empty() {
        return Err("批次名称无效".into());
    }
    if cleaned.chars().count() > 120 {
        return Err("批次名称过长（最多120字符）".into());
    }
    Ok(cleaned)
}

/// Empty name → YYYYMMDDHHmmss; else sanitized name. Collision → name_YYYYMMDDHHmmss.
fn resolve_batch_id(app: &tauri::AppHandle, batch_name: Option<String>) -> Result<String, String> {
    let trimmed = batch_name.unwrap_or_default();
    let trimmed = trimmed.trim();
    let base = if trimmed.is_empty() {
        now_id()
    } else {
        sanitize_batch_id(trimmed)?
    };
    let meta = metadata_root(app)?;
    let path = meta.join(format!("{base}.json"));
    if path.exists() {
        Ok(format!("{base}_{}", now_id()))
    } else {
        Ok(base)
    }
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

/// Feed the same head/tail windows as [`compute_md5_quick_legacy`] while bytes stream past `pos`.
fn feed_legacy_quick(hasher: &mut Md5, data: &[u8], pos: u64, size: u64) {
    if size == 0 || data.is_empty() {
        return;
    }
    let end = pos + data.len() as u64;
    if size <= SAMPLE_WINDOW * 2 {
        hasher.update(data);
        return;
    }
    let head_end = SAMPLE_WINDOW;
    if pos < head_end {
        let b = end.min(head_end);
        if b > pos {
            hasher.update(&data[..(b - pos) as usize]);
        }
    }
    let tail_start = size - SAMPLE_WINDOW;
    if end > tail_start {
        let a = pos.max(tail_start);
        let b = end.min(size);
        if b > a {
            hasher.update(&data[(a - pos) as usize..(b - pos) as usize]);
        }
    }
}

/// Copy `src` to `dest` and return `(bytes, legacy quick md5)` from that single read.
fn copy_with_legacy_quick(src: &Path, dest: &Path) -> Result<(u64, String), String> {
    let meta = fs::metadata(src).map_err(|e| format!("元数据失败 {}: {e}", src.display()))?;
    let size = meta.len();
    let mut input = File::open(src).map_err(|e| format!("打开失败 {}: {e}", src.display()))?;
    let mut output = File::create(dest).map_err(|e| format!("创建目标失败 {}: {e}", dest.display()))?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; hashutil::READ_BUF_SIZE];
    let mut pos = 0u64;
    loop {
        let n = input.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        output
            .write_all(&buf[..n])
            .map_err(|e| format!("写入失败: {e}"))?;
        feed_legacy_quick(&mut hasher, &buf[..n], pos, size);
        pos += n as u64;
    }
    hasher.update(size.to_string().as_bytes());
    Ok((size, format!("{:x}", hasher.finalize())))
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


/// Parse optional JS-style `/pattern/flags` (e.g. `/\.exe$/i`).
/// Returns `(pattern, flags)` when delimiters are present; otherwise `None`.
fn parse_slash_delimited_regex(raw: &str) -> Option<(&str, &str)> {
    let s = raw.trim();
    if !s.starts_with('/') || s.len() < 2 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 1usize;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            i += 1;
            continue;
        }
        if b == b'/' {
            let pattern = &s[1..i];
            let flags = &s[i + 1..];
            // Require a non-empty pattern; reject bare `//flags`.
            if pattern.is_empty() {
                return None;
            }
            return Some((pattern, flags));
        }
        i += 1;
    }
    None
}

fn compile_name_regex(pattern: &Option<String>) -> Result<Option<regex::Regex>, String> {
    let Some(raw) = pattern.as_ref() else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let (body, flags) = match parse_slash_delimited_regex(raw) {
        Some((p, f)) => (p.to_string(), f.to_string()),
        None => (raw.to_string(), String::new()),
    };
    let mut builder = regex::RegexBuilder::new(&body);
    for ch in flags.chars() {
        match ch {
            'i' | 'I' => {
                builder.case_insensitive(true);
            }
            'm' | 'M' => {
                builder.multi_line(true);
            }
            's' | 'S' => {
                builder.dot_matches_new_line(true);
            }
            'x' | 'X' => {
                builder.ignore_whitespace(true);
            }
            // JS `g`/`u`/`y` have no effect for filename is_match; ignore quietly.
            'g' | 'G' | 'u' | 'U' | 'y' | 'Y' => {}
            other => {
                return Err(format!("不支持的正则标志: {other}"));
            }
        }
    }
    builder
        .build()
        .map(Some)
        .map_err(|e| format!("文件名正则无效: {e}"))
}

fn filter_collected_by_name(
    collected: Vec<(PathBuf, PathBuf)>,
    re: &Option<regex::Regex>,
    exclude: bool,
) -> Vec<(PathBuf, PathBuf)> {
    let Some(re) = re else {
        return collected;
    };
    collected
        .into_iter()
        .filter(|(abs, _)| {
            let name = abs
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            tarutil::name_matches(&name, re, exclude)
        })
        .collect()
}

fn is_controlled_tar_file(path: &Path) -> bool {
    let name_l = path
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !name_l.ends_with(".tar") || !path.is_file() {
        return false;
    }
    let Some(root) = disk::drive_root_of(path) else {
        return false;
    };
    if !disk::is_backup_disk(&root) {
        return false;
    }
    let Ok(rel) = vault::normalize_rel_path(&root, path) else {
        return false;
    };
    vault::controlled_rel_path_exists(&root, &rel).unwrap_or(false)
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
        return Err("只能标记盘符根目录为受控盘（例如 E:\\），不能标记子目录".into());
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
    vault::verify_controlled(&root, "full", rel_paths, None)
}

#[tauri::command]
fn verify_controlled_quick(
    drive: String,
    rel_paths: Option<Vec<String>>,
) -> Result<ControlledVerifyReport, String> {
    let root = disk::normalize_drive_root(&drive)?;
    vault::verify_controlled(&root, "quick", rel_paths, None)
}

#[tauri::command]
fn list_dir(path: String) -> Result<Vec<DirEntryInfo>, String> {
    if path.is_empty() {
        return list_drives().map(|ds| {
            ds.into_iter()
                .map(|d| DirEntryInfo {
                    name: d.path.clone(),
                    path: d.path.clone(),
                    is_dir: true,
                    size: 0,
                    is_backup_disk: d.is_backup_disk,
                    // Drive root row: marked backup disk => controlled (counts filled async).
                    is_controlled: d.is_backup_disk,
                    controlled_count: None,
                    total_files: None,
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

    // Controlled-status overlay when browsing a backup disk. Paths only.
    let drive_root = disk::drive_root_of(&p);
    let controlled_index = if let Some(ref root) = drive_root {
        if disk::is_backup_disk(root) {
            vault::list_controlled_rel_paths(root)
                .ok()
                .map(vault::ControlledPathIndex::from_paths)
        } else {
            None
        }
    } else {
        None
    };

    let mut items = Vec::new();
    let entries = fs::read_dir(&p).map_err(|e| format!("无法读取目录（可能无权限）: {e}"))?;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case(disk::META_DIR) {
            continue;
        }
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let is_dir = ft.is_dir();
        let ep = entry.path();
        let size = if is_dir {
            0
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };

        let is_controlled = if let (Some(index), Some(root)) = (&controlled_index, &drive_root) {
            match vault::normalize_rel_path(root, &ep) {
                Ok(rel) => {
                    let rel_l = rel.to_lowercase();
                    if is_dir {
                        index.dir_has_controlled(&rel_l)
                    } else {
                        index.contains_file(&rel_l)
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
            controlled_count: None,
            total_files: None,
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

fn backup_paths_inner(
    app: tauri::AppHandle,
    sources: Vec<String>,
    dest: String,
    batch_name: Option<String>,
    progress_job_id: Option<String>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    name_regex: Option<String>,
    regex_exclude: bool,
    pack_as_tar: bool,
) -> Result<BackupBatch, String> {
    if sources.is_empty() {
        return Err("未选择任何源路径".into());
    }
    if dest.trim().is_empty() {
        return Err("目标目录为空".into());
    }
    let re = compile_name_regex(&name_regex)?;
    let dest_root = PathBuf::from(&dest);
    fs::create_dir_all(&dest_root).map_err(|e| format!("创建目标目录失败: {e}"))?;

    let batch_id = resolve_batch_id(&app, batch_name)?;
    let mut files_meta: Vec<FileMeta> = Vec::new();
    let mut progress_idx = 0usize;
    let mut all_collected: Vec<(PathBuf, PathBuf)> = Vec::new();

    for src_str in &sources {
        if let Some(flag) = cancel.as_ref() {
            if job::is_cancelled(flag) {
                break;
            }
        }
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
        collected = filter_collected_by_name(collected, &re, regex_exclude);
        all_collected.extend(collected);
    }

    if pack_as_tar {
        if all_collected.is_empty() && files_meta.iter().all(|f| f.error.is_some()) {
            // keep error metas only
        } else if all_collected.is_empty() {
            return Err("正则过滤后没有可备份的文件".into());
        } else {
            let tar_name = format!("{batch_id}.tar");
            let tar_path = dest_root.join(&tar_name);
            let total = all_collected.len();
            let app_prog = app.clone();
            let jid = progress_job_id.clone();
            let cancel_c = cancel.clone();
            let mut gate = job::ProgressGate::new();
            let size = tarutil::pack_files(&tar_path, &all_collected, |idx, rel_s| {
                if let Some(flag) = cancel_c.as_ref() {
                    if job::is_cancelled(flag) {
                        return;
                    }
                }
                if let Some(jid) = jid.as_ref() {
                    if gate.due(idx == total) {
                        let _ = app_prog.emit(
                            "backup-job-progress",
                            GenericProgress {
                                job_id: jid.clone(),
                                phase: "packing".into(),
                                current: idx,
                                total,
                                rel_path: Some(rel_s.to_string()),
                                message: format!("正在打包 ({idx}/{total}): {rel_s}"),
                            },
                        );
                    }
                }
            })?;
            if let Some(flag) = cancel.as_ref() {
                if job::is_cancelled(flag) {
                    let _ = fs::remove_file(&tar_path);
                    return Err("已取消".into());
                }
            }
            let pair = compute_md5_pair(
                &tar_path,
                hashutil::DEFAULT_SAMPLE_RATIO,
                hashutil::DEFAULT_SAMPLE_CHUNK_MB,
            )
            .ok();
            let quick = compute_md5_quick_legacy(&tar_path).ok();
            let dest_s = tar_path.to_string_lossy().to_string();
            files_meta.push(FileMeta {
                rel_path: tar_name.clone(),
                src_path: "(archive)".into(),
                dest_path: dest_s.clone(),
                size,
                md5_full: pair.as_ref().map(|p| p.md5.clone()),
                md5_quick: quick,
                error: None,
            });
            // Register controlled metadata for the archive when dest is a controlled disk.
            if let Some(root) = disk::drive_root_of(&tar_path) {
                if disk::is_backup_disk(&root) {
                    if let Some(jid) = progress_job_id.as_ref() {
                        let _ = app.emit(
                            "backup-job-progress",
                            GenericProgress {
                                job_id: jid.clone(),
                                phase: "indexing".into(),
                                current: total,
                                total,
                                rel_path: Some(tar_name.clone()),
                                message: format!("登记受控元数据: {tar_name}"),
                            },
                        );
                    }
                    let registered = if let Some(pair) = pair.as_ref() {
                        vault::upsert_prehashed(
                            &root,
                            &tar_path,
                            &pair.md5,
                            &pair.fast_md5,
                            hashutil::DEFAULT_SAMPLE_RATIO,
                            hashutil::DEFAULT_SAMPLE_CHUNK_MB,
                        )
                    } else {
                        Err("计算归档哈希失败".into())
                    };
                    match registered {
                        Ok(_) => {}
                        Err(e) => {
                            // Non-fatal: backup succeeded; surface in meta error note
                            if let Some(last) = files_meta.last_mut() {
                                last.error = Some(format!("已打包，但登记受控失败: {e}"));
                            }
                        }
                    }
                }
            }
        }
    } else {
        let copy_total = all_collected.len();
        let mut copy_gate = job::ProgressGate::new();
        for (abs, rel) in all_collected {
            if let Some(flag) = cancel.as_ref() {
                if job::is_cancelled(flag) {
                    break;
                }
            }
            progress_idx += 1;
            let rel_s = rel.to_string_lossy().to_string();
            if let Some(jid) = progress_job_id.as_ref() {
                if copy_gate.due(progress_idx == copy_total) {
                    let _ = app.emit(
                        "backup-job-progress",
                        GenericProgress {
                            job_id: jid.clone(),
                            phase: "copying".into(),
                            current: progress_idx,
                            total: copy_total,
                            rel_path: Some(rel_s.clone()),
                            message: format!("正在复制 ({progress_idx}/{copy_total}): {rel_s}"),
                        },
                    );
                }
            }
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
            match copy_with_legacy_quick(&abs, &dest_path) {
                Ok((size, quick)) => {
                    files_meta.push(FileMeta {
                        rel_path: rel.to_string_lossy().to_string(),
                        src_path: abs.to_string_lossy().to_string(),
                        dest_path: dest_path.to_string_lossy().to_string(),
                        size,
                        md5_full: None,
                        md5_quick: Some(quick),
                        error: None,
                    });
                }
                Err(e) => {
                    let _ = fs::remove_file(&dest_path);
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
        pack_as_tar,
        name_regex: name_regex
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        regex_exclude,
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

fn verify_backup_inner(
    app: tauri::AppHandle,
    batch_id: String,
    mode: String,
    progress_job_id: Option<String>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VerifyReport, String> {
    let batch = load_batch(app.clone(), batch_id.clone())?;
    let use_full = mode == "full" || mode == "完整" || mode == "完整校验";
    let mut items = Vec::new();
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut missing = 0usize;
    let mut errors = 0usize;

    let total = batch.files.len();
    let mut verify_gate = job::ProgressGate::new();
    for (i, f) in batch.files.iter().enumerate() {
        if let Some(flag) = cancel.as_ref() {
            if job::is_cancelled(flag) {
                break;
            }
        }
        let current = i + 1;
        if let Some(jid) = progress_job_id.as_ref() {
            if verify_gate.due(current == total) {
                let _ = app.emit(
                    "verify-job-progress",
                    GenericProgress {
                        job_id: jid.clone(),
                        phase: "verifying".into(),
                        current,
                        total,
                        rel_path: Some(f.rel_path.clone()),
                        message: format!("正在校验 ({current}/{total}): {}", f.rel_path),
                    },
                );
            }
        }
        if f.error.is_some() {
            errors += 1;
            items.push(VerifyItem {
                rel_path: f.rel_path.clone(),
                src_path: f.src_path.clone(),
                dest_path: f.dest_path.clone(),
                src_hash: None,
                dest_hash: None,
                ok: false,
                status: "error".into(),
                message: format!("备份时失败: {}", f.error.clone().unwrap_or_default()),
            });
            continue;
        }
        // Tar archive backup: only the destination .tar exists; check against stored hash.
        if f.src_path == "(archive)" {
            let dest = Path::new(&f.dest_path);
            if !dest.exists() {
                missing += 1;
                items.push(VerifyItem {
                    rel_path: f.rel_path.clone(),
                    src_path: f.src_path.clone(),
                    dest_path: f.dest_path.clone(),
                    src_hash: f.md5_quick.clone().or_else(|| f.md5_full.clone()),
                    dest_hash: None,
                    ok: false,
                    status: "missing".into(),
                    message: "目标归档缺失".into(),
                });
                continue;
            }
            let dest_hash_res = if use_full {
                compute_md5_full(dest)
            } else {
                compute_md5_quick_legacy(dest)
            };
            match dest_hash_res {
                Ok(dh) => {
                    let expected = if use_full {
                        f.md5_full.clone().or_else(|| f.md5_quick.clone())
                    } else {
                        f.md5_quick.clone().or_else(|| f.md5_full.clone())
                    };
                    let (ok, message) = match &expected {
                        Some(exp) if exp == &dh => (true, "通过".to_string()),
                        Some(_) => (false, "哈希不一致".to_string()),
                        None => (true, "归档可读（无存哈希）".to_string()),
                    };
                    if ok {
                        passed += 1;
                    } else {
                        failed += 1;
                    }
                    items.push(VerifyItem {
                        rel_path: f.rel_path.clone(),
                        src_path: f.src_path.clone(),
                        dest_path: f.dest_path.clone(),
                        src_hash: expected,
                        dest_hash: Some(dh),
                        ok,
                        status: if ok { "pass".into() } else { "fail".into() },
                        message,
                    });
                }
                Err(e) => {
                    errors += 1;
                    items.push(VerifyItem {
                        rel_path: f.rel_path.clone(),
                        src_path: f.src_path.clone(),
                        dest_path: f.dest_path.clone(),
                        src_hash: None,
                        dest_hash: None,
                        ok: false,
                        status: "error".into(),
                        message: e,
                    });
                }
            }
            continue;
        }


        let src = Path::new(&f.src_path);
        let dest = Path::new(&f.dest_path);
        if !src.exists() || !dest.exists() {
            missing += 1;
            let msg = match (src.exists(), dest.exists()) {
                (false, false) => "源与目标文件均缺失",
                (false, true) => "源文件缺失",
                (true, false) => "目标文件缺失",
                _ => "源或目标文件缺失",
            };
            items.push(VerifyItem {
                rel_path: f.rel_path.clone(),
                src_path: f.src_path.clone(),
                dest_path: f.dest_path.clone(),
                src_hash: None,
                dest_hash: None,
                ok: false,
                status: "missing".into(),
                message: msg.into(),
            });
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
                    status: if ok { "pass".into() } else { "fail".into() },
                    message: if ok {
                        "通过".into()
                    } else {
                        "哈希不一致".into()
                    },
                });
            }
            (Err(e), _) | (_, Err(e)) => {
                errors += 1;
                items.push(VerifyItem {
                    rel_path: f.rel_path.clone(),
                    src_path: f.src_path.clone(),
                    dest_path: f.dest_path.clone(),
                    src_hash: None,
                    dest_hash: None,
                    ok: false,
                    status: "error".into(),
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
        missing,
        errors,
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
fn backup_paths(
    app: tauri::AppHandle,
    sources: Vec<String>,
    dest: String,
    batch_name: Option<String>,
    name_regex: Option<String>,
    regex_exclude: Option<bool>,
    pack_as_tar: Option<bool>,
) -> Result<BackupBatch, String> {
    backup_paths_inner(
        app,
        sources,
        dest,
        batch_name,
        None,
        None,
        name_regex,
        regex_exclude.unwrap_or(false),
        pack_as_tar.unwrap_or(false),
    )
}

#[tauri::command]
fn verify_backup(
    app: tauri::AppHandle,
    batch_id: String,
    mode: String,
) -> Result<VerifyReport, String> {
    verify_backup_inner(app, batch_id, mode, None, None)
}

#[tauri::command]
fn cancel_backup_job() -> Result<bool, String> {
    Ok(job::cancel_kinds(&["backup"]))
}

#[tauri::command]
fn cancel_verify_job() -> Result<bool, String> {
    Ok(job::cancel_kinds(&["controlled-full", "controlled-quick", "batch", "batch-full", "batch-quick", "verify"]))
}

#[tauri::command]
fn start_backup(
    app: tauri::AppHandle,
    sources: Vec<String>,
    dest: String,
    batch_name: Option<String>,
    name_regex: Option<String>,
    regex_exclude: Option<bool>,
    pack_as_tar: Option<bool>,
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
            let result = backup_paths_inner(app.clone(), sources, dest, batch_name, Some(job_id.clone()), Some(cancel.clone()), name_regex, regex_exclude.unwrap_or(false), pack_as_tar.unwrap_or(false));
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
            let root = match disk::normalize_drive_root(&drive) {
                Ok(r) => r,
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
            };
            let job_id_for_cb = job_id.clone();
            let app_for_cb = app.clone();
            let cancel_for_cb = cancel.clone();
            let mut gate = job::ProgressGate::new();
            let result = vault::verify_controlled(
                &root,
                &mode,
                filter,
                Some(&mut |cur, total, path| {
                    let cancelled = job::is_cancelled(&cancel_for_cb);
                    if gate.due(cancelled || cur == total) {
                        let _ = app_for_cb.emit(
                            "verify-job-progress",
                            GenericProgress {
                                job_id: job_id_for_cb.clone(),
                                phase: "verifying".into(),
                                current: cur,
                                total,
                                rel_path: Some(path.to_string()),
                                message: format!("正在校验 ({cur}/{total}): {path}"),
                            },
                        );
                    }
                    !cancelled
                }),
            );
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
    let use_full = mode == "full" || mode == "完整" || mode == "完整校验";
    let kind = if use_full { "batch-full" } else { "batch-quick" };
    let (job_id, cancel) = job::register_job(kind);
    let job_id_ret = job_id.clone();
    let kind_owned = kind.to_string();
    std::thread::Builder::new()
        .name("datavault-verify-batch".into())
        .spawn(move || {
            let kind = kind_owned;
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
                        kind: kind.clone(),
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
            match verify_backup_inner(app.clone(), batch_id, mode, Some(job_id.clone()), Some(cancel.clone())) {
                Ok(report) => {
                    let cancelled = job::is_cancelled(&cancel);
                    let msg = if cancelled {
                        format!(
                            "批次校验已取消：通过 {}，失败 {}，缺失 {}，错误 {}",
                            report.passed, report.failed, report.missing, report.errors
                        )
                    } else {
                        format!(
                            "批次校验完成：通过 {}，失败 {}，缺失 {}，错误 {}",
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
                            kind: kind.clone(),
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
                            kind: kind.clone(),
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
        kind: kind.to_string(),
    })
}



/// Monotonic job id for directory file-count updates. Newer start cancels older.
static DIR_COUNTS_JOB: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize)]
pub struct DirCountUpdate {
    pub job_id: u64,
    pub path: String,
    pub controlled_count: u64,
    pub total_files: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirCountsFinished {
    pub job_id: u64,
    pub ok: bool,
    pub cancelled: bool,
    pub message: String,
}

/// Recursively count files under `dir`, skipping `.datavault`. Returns None if job cancelled.
fn count_files_under(dir: &Path, my_id: u64) -> Option<u64> {
    let mut n = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        if DIR_COUNTS_JOB.load(Ordering::SeqCst) != my_id {
            return None;
        }
        let rd = match fs::read_dir(&p) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            if DIR_COUNTS_JOB.load(Ordering::SeqCst) != my_id {
                return None;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            if name.eq_ignore_ascii_case(disk::META_DIR) {
                continue;
            }
            let is_dir = ent.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir {
                stack.push(ent.path());
            } else {
                n += 1;
            }
        }
    }
    Some(n)
}

fn controlled_count_for(index: &vault::ControlledPathIndex, root: &Path, dir: &Path) -> u64 {
    let root_s = root
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\");
    let dir_s = dir
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\");
    if dir_s.eq_ignore_ascii_case(&root_s) {
        return index.len();
    }
    match vault::normalize_rel_path(root, dir) {
        Ok(rel) => index.count_under(&rel.to_lowercase()),
        Err(_) => 0,
    }
}

fn ensure_index<'a>(
    root: &Path,
    cache: &'a mut std::collections::HashMap<String, vault::ControlledPathIndex>,
) -> Option<&'a vault::ControlledPathIndex> {
    if !disk::is_backup_disk(root) {
        return None;
    }
    let key = root.to_string_lossy().to_string();
    if !cache.contains_key(&key) {
        let rels = vault::list_controlled_rel_paths(root).unwrap_or_default();
        cache.insert(key.clone(), vault::ControlledPathIndex::from_paths(rels));
    }
    cache.get(&key)
}

/// Start async counts for controlled directories. At most one job; newer call cancels previous.
#[tauri::command]
fn start_dir_file_counts(app: tauri::AppHandle, paths: Vec<String>) -> Result<u64, String> {
    let job_id = DIR_COUNTS_JOB.fetch_add(1, Ordering::SeqCst) + 1;
    let paths: Vec<String> = paths
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect();
    if paths.is_empty() {
        return Ok(job_id);
    }
    std::thread::Builder::new()
        .name("datavault-dir-counts".into())
        .spawn(move || {
            let mut index_cache = std::collections::HashMap::new();
            for path in paths {
                if DIR_COUNTS_JOB.load(Ordering::SeqCst) != job_id {
                    let _ = app.emit(
                        "dir-counts-finished",
                        DirCountsFinished {
                            job_id,
                            ok: false,
                            cancelled: true,
                            message: "已取消".into(),
                        },
                    );
                    return;
                }
                let p = PathBuf::from(&path);
                if !p.is_dir() {
                    continue;
                }
                let controlled = if let Some(root) = disk::drive_root_of(&p) {
                    if let Some(index) = ensure_index(&root, &mut index_cache) {
                        controlled_count_for(index, &root, &p)
                    } else {
                        0
                    }
                } else {
                    0
                };
                let Some(total) = count_files_under(&p, job_id) else {
                    let _ = app.emit(
                        "dir-counts-finished",
                        DirCountsFinished {
                            job_id,
                            ok: false,
                            cancelled: true,
                            message: "已取消".into(),
                        },
                    );
                    return;
                };
                let _ = app.emit(
                    "dir-counts-update",
                    DirCountUpdate {
                        job_id,
                        path,
                        controlled_count: controlled,
                        total_files: total,
                    },
                );
            }
            if DIR_COUNTS_JOB.load(Ordering::SeqCst) == job_id {
                let _ = app.emit(
                    "dir-counts-finished",
                    DirCountsFinished {
                        job_id,
                        ok: true,
                        cancelled: false,
                        message: "完成".into(),
                    },
                );
            }
        })
        .map_err(|e| format!("无法启动目录计数线程: {e}"))?;
    Ok(job_id)
}



#[tauri::command]
fn default_datavault_export_name(drive: String) -> Result<String, String> {
    metabackup::default_export_name(&drive)
}

#[tauri::command]
fn export_datavault_metadata(drive: String, zip_path: String) -> Result<ExportResult, String> {
    metabackup::export_datavault_metadata(&drive, &zip_path)
}

#[tauri::command]
fn verify_datavault_backup_zip(zip_path: String) -> Result<MetaVerifyReport, String> {
    metabackup::verify_datavault_backup_zip(&zip_path)
}

#[tauri::command]
fn restore_datavault_metadata(drive: String, zip_path: String) -> Result<RestoreResult, String> {
    metabackup::restore_datavault_metadata(&drive, &zip_path)
}

#[tauri::command]
fn list_tar_entries(path: String, prefix: Option<String>) -> Result<Vec<TarEntryInfo>, String> {
    let p = PathBuf::from(&path);
    if !is_controlled_tar_file(&p) {
        return Err("仅可展开已登记为受控的 .tar 归档".into());
    }
    tarutil::list_entries(&p, prefix.as_deref().unwrap_or(""))
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
            list_tar_entries,
            default_datavault_export_name,
            export_datavault_metadata,
            verify_datavault_backup_zip,
            restore_datavault_metadata,
            start_dir_file_counts,
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

#[cfg(test)]
mod copy_tests {
    use super::{compute_md5_quick_legacy, copy_with_legacy_quick};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn check(len: usize) {
        let dir = std::env::temp_dir().join(format!(
            "dv-copy-{}-{}",
            len,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("s.bin");
        let dest = dir.join("d.bin");
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let (n, quick) = copy_with_legacy_quick(&src, &dest).unwrap();
        assert_eq!(n, len as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(quick, compute_md5_quick_legacy(&src).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quick_hash_matches_legacy_while_copying() {
        check(0);
        check(100);
        check(70_000);
        check(200_000);
    }
}

#[cfg(test)]
mod name_regex_tests {
    use super::{compile_name_regex, parse_slash_delimited_regex};
    use crate::tarutil;

    #[test]
    fn parse_slash_form_with_flags() {
        let (p, f) = parse_slash_delimited_regex(r"/\.exe$/i").unwrap();
        assert_eq!(p, r"\.exe$");
        assert_eq!(f, "i");
    }

    #[test]
    fn bare_pattern_not_slash_form() {
        assert!(parse_slash_delimited_regex(r"\.exe$").is_none());
    }

    #[test]
    fn exclude_exe_case_insensitive_slash_form() {
        let re = compile_name_regex(&Some(r"/\.exe$/i".into()))
            .unwrap()
            .unwrap();
        assert!(tarutil::name_matches("tool.exe", &re, true) == false);
        assert!(tarutil::name_matches("TOOL.EXE", &re, true) == false);
        assert!(tarutil::name_matches("readme.txt", &re, true));
        assert!(tarutil::name_matches("lib.dll", &re, true));
    }

    #[test]
    fn include_pdf_bare() {
        let re = compile_name_regex(&Some(r"\.pdf$".into()))
            .unwrap()
            .unwrap();
        assert!(tarutil::name_matches("a.pdf", &re, false));
        assert!(!tarutil::name_matches("a.PDF", &re, false));
        assert!(!tarutil::name_matches("a.txt", &re, false));
    }

    #[test]
    fn literal_slash_form_without_flags_still_works() {
        // Unclosed /pattern is treated as raw Rust regex (legacy).
        let re = compile_name_regex(&Some(r"/foo".into())).unwrap().unwrap();
        assert!(re.is_match("/foo"));
    }
}
