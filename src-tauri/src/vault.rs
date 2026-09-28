//! SQLite vault.db — controlled_files table on the backup disk.

use crate::disk::{self, shanghai_now_iso};
use crate::hashutil::{
    compute_fast_md5, compute_md5_full, DEFAULT_SAMPLE_CHUNK_MB, DEFAULT_SAMPLE_RATIO,
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ControlledFile {
    pub rel_path: String,
    pub size: i64,
    pub mtime: i64,
    pub md5: String,
    pub fast_md5: String,
    pub sample_ratio: f64,
    pub sample_chunk_mb: i64,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ControlledVerifyItem {
    pub rel_path: String,
    pub status: String, // pass | fail | missing | error
    pub message: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub size_changed: bool,
    pub mtime_changed: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ControlledVerifyReport {
    pub drive_root: String,
    pub mode: String,
    pub items: Vec<ControlledVerifyItem>,
    pub passed: usize,
    pub failed: usize,
    pub missing: usize,
    pub errors: usize,
}

pub fn ensure_db(drive_root: &Path) -> Result<(), String> {
    let meta = disk::meta_dir(drive_root);
    fs::create_dir_all(&meta).map_err(|e| format!("创建 .datavault 失败: {e}"))?;
    let db_path = disk::vault_db_path(drive_root);
    let conn = Connection::open(&db_path).map_err(|e| format!("打开 vault.db 失败: {e}"))?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS controlled_files (
          rel_path TEXT PRIMARY KEY,
          size INTEGER NOT NULL,
          mtime INTEGER NOT NULL,
          md5 TEXT NOT NULL,
          fast_md5 TEXT NOT NULL,
          sample_ratio REAL NOT NULL DEFAULT 0.10,
          sample_chunk_mb INTEGER NOT NULL DEFAULT 100,
          updated_at TEXT NOT NULL
        );
        "#,
    )
    .map_err(|e| format!("建表失败: {e}"))?;
    Ok(())
}

fn open_db(drive_root: &Path) -> Result<Connection, String> {
    ensure_db(drive_root)?;
    let db_path = disk::vault_db_path(drive_root);
    Connection::open(db_path).map_err(|e| format!("打开 vault.db 失败: {e}"))
}

/// Normalize path relative to drive root:
/// - strip drive prefix
/// - use `\` separators
/// - reject `..` and absolute components
/// - reject paths under `.datavault`
pub fn normalize_rel_path(drive_root: &Path, abs: &Path) -> Result<String, String> {
    let abs_norm = abs.to_string_lossy().replace('/', "\\");
    let root_s = drive_root
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\");
    let root_with_sep = format!(r"{root_s}\");

    let rel_raw = if abs_norm.eq_ignore_ascii_case(&root_s)
        || abs_norm.eq_ignore_ascii_case(&root_with_sep)
    {
        return Err("不能将盘根本身登记为受控文件".into());
    } else if abs_norm.len() > root_with_sep.len()
        && abs_norm[..root_with_sep.len()].eq_ignore_ascii_case(&root_with_sep)
    {
        abs_norm[root_with_sep.len()..].to_string()
    } else if let Ok(stripped) = abs.strip_prefix(drive_root) {
        stripped.to_string_lossy().replace('/', "\\")
    } else {
        return Err(format!(
            "文件不在盘根 {} 下: {}",
            drive_root.display(),
            abs.display()
        ));
    };

    let rel_path = PathBuf::from(rel_raw);
    let mut parts: Vec<String> = Vec::new();
    for c in rel_path.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("非法相对路径（含 ..）: {}", abs.display()));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("非法相对路径: {}", abs.display()));
            }
        }
    }
    if parts.is_empty() {
        return Err("相对路径为空".into());
    }
    if parts[0].eq_ignore_ascii_case(disk::META_DIR) {
        return Err(".datavault 目录下的文件不可登记为受控文件".into());
    }
    Ok(parts.join("\\"))
}

fn file_mtime_secs(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn upsert_controlled_file(
    drive_root: &Path,
    abs_path: &Path,
    sample_ratio: f64,
    sample_chunk_mb: i64,
) -> Result<ControlledFile, String> {
    if !disk::is_backup_disk(drive_root) {
        return Err("当前盘不是 DataVault 备份盘，请先标记".into());
    }
    if !abs_path.is_file() {
        return Err(format!("不是文件: {}", abs_path.display()));
    }
    let rel_path = normalize_rel_path(drive_root, abs_path)?;
    let meta = fs::metadata(abs_path).map_err(|e| format!("元数据失败: {e}"))?;
    let size = meta.len() as i64;
    let mtime = file_mtime_secs(&meta);
    let md5 = compute_md5_full(abs_path)?;
    let fast_md5 = compute_fast_md5(abs_path, sample_ratio, sample_chunk_mb)?;
    let updated_at = shanghai_now_iso();

    let conn = open_db(drive_root)?;
    conn.execute(
        r#"
        INSERT INTO controlled_files
          (rel_path, size, mtime, md5, fast_md5, sample_ratio, sample_chunk_mb, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(rel_path) DO UPDATE SET
          size=excluded.size,
          mtime=excluded.mtime,
          md5=excluded.md5,
          fast_md5=excluded.fast_md5,
          sample_ratio=excluded.sample_ratio,
          sample_chunk_mb=excluded.sample_chunk_mb,
          updated_at=excluded.updated_at
        "#,
        params![
            rel_path,
            size,
            mtime,
            md5,
            fast_md5,
            sample_ratio,
            sample_chunk_mb,
            updated_at
        ],
    )
    .map_err(|e| format!("写入 controlled_files 失败: {e}"))?;

    Ok(ControlledFile {
        rel_path,
        size,
        mtime,
        md5,
        fast_md5,
        sample_ratio,
        sample_chunk_mb,
        updated_at,
    })
}

pub fn add_controlled_files(
    drive_root: &Path,
    paths: &[String],
) -> Result<Vec<ControlledFile>, String> {
    let existing: std::collections::HashSet<String> = list_controlled_files(drive_root)
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.rel_path.to_lowercase())
        .collect();
    let mut out = Vec::new();
    let files = expand_paths_to_files(drive_root, paths)?;
    for f in files {
        let rel = match normalize_rel_path(drive_root, &f) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if existing.contains(&rel.to_lowercase()) {
            continue; // already controlled — skip re-hash
        }
        out.push(upsert_controlled_file(
            drive_root,
            &f,
            DEFAULT_SAMPLE_RATIO,
            DEFAULT_SAMPLE_CHUNK_MB,
        )?);
    }
    Ok(out)
}


/// Expand user-selected paths into concrete files (directories recurse; skip `.datavault`).
pub fn expand_paths_to_files(drive_root: &Path, paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for p in paths {
        let abs = PathBuf::from(p);
        if abs.is_dir() {
            collect_files_under(&abs, &mut out)?;
        } else if abs.is_file() {
            // Reject files under .datavault via normalize
            let _ = normalize_rel_path(drive_root, &abs)?;
            out.push(abs);
        } else {
            return Err(format!("路径不存在: {}", abs.display()));
        }
    }
    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| {
        let key = p.to_string_lossy().to_lowercase();
        seen.insert(key)
    });
    Ok(out)
}

pub fn collect_files_under(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("读取目录失败 {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("目录项错误: {e}"))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case(disk::META_DIR) {
            continue;
        }
        if path.is_dir() {
            collect_files_under(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

pub fn list_controlled_files(drive_root: &Path) -> Result<Vec<ControlledFile>, String> {
    if !disk::is_backup_disk(drive_root) {
        return Err("当前盘不是 DataVault 备份盘".into());
    }
    let conn = open_db(drive_root)?;
    let mut stmt = conn
        .prepare(
            r#"
            SELECT rel_path, size, mtime, md5, fast_md5, sample_ratio, sample_chunk_mb, updated_at
            FROM controlled_files
            ORDER BY rel_path COLLATE NOCASE
            "#,
        )
        .map_err(|e| format!("查询失败: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ControlledFile {
                rel_path: row.get(0)?,
                size: row.get(1)?,
                mtime: row.get(2)?,
                md5: row.get(3)?,
                fast_md5: row.get(4)?,
                sample_ratio: row.get(5)?,
                sample_chunk_mb: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("查询失败: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("行读取失败: {e}"))?);
    }
    Ok(out)
}

pub fn verify_controlled(
    drive_root: &Path,
    mode: &str,
    rel_paths: Option<Vec<String>>,
) -> Result<ControlledVerifyReport, String> {
    if !disk::is_backup_disk(drive_root) {
        return Err("当前盘不是 DataVault 备份盘".into());
    }
    let use_full = matches!(mode, "full" | "完整" | "完整校验");
    let all = list_controlled_files(drive_root)?;
    let targets: Vec<ControlledFile> = if let Some(filter) = rel_paths {
        if filter.is_empty() {
            all
        } else {
            let set: std::collections::HashSet<_> =
                filter.into_iter().map(|s| s.replace('/', "\\")).collect();
            all.into_iter()
                .filter(|f| set.contains(&f.rel_path))
                .collect()
        }
    } else {
        all
    };

    let mut items = Vec::new();
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut missing = 0usize;
    let mut errors = 0usize;

    for row in targets {
        let abs = drive_root.join(&row.rel_path);
        if !abs.exists() {
            missing += 1;
            items.push(ControlledVerifyItem {
                rel_path: row.rel_path,
                status: "missing".into(),
                message: "文件不在盘上".into(),
                expected: None,
                actual: None,
                size_changed: false,
                mtime_changed: false,
            });
            continue;
        }
        let meta = match fs::metadata(&abs) {
            Ok(m) => m,
            Err(e) => {
                errors += 1;
                items.push(ControlledVerifyItem {
                    rel_path: row.rel_path,
                    status: "error".into(),
                    message: format!("无法读取元数据: {e}"),
                    expected: None,
                    actual: None,
                    size_changed: false,
                    mtime_changed: false,
                });
                continue;
            }
        };
        let cur_size = meta.len() as i64;
        let cur_mtime = file_mtime_secs(&meta);
        let size_changed = cur_size != row.size;
        let mtime_changed = cur_mtime != row.mtime;

        let hash_result = if use_full {
            compute_md5_full(&abs)
        } else {
            compute_fast_md5(&abs, row.sample_ratio, row.sample_chunk_mb)
        };

        match hash_result {
            Ok(actual) => {
                let expected = if use_full {
                    row.md5.clone()
                } else {
                    row.fast_md5.clone()
                };
                let ok = actual == expected;
                let mut msg = if ok {
                    "通过".to_string()
                } else {
                    "哈希不一致".to_string()
                };
                if size_changed {
                    msg.push_str("；大小已变化");
                }
                if mtime_changed {
                    msg.push_str("；修改时间已变化");
                }
                if ok {
                    passed += 1;
                    items.push(ControlledVerifyItem {
                        rel_path: row.rel_path,
                        status: "pass".into(),
                        message: msg,
                        expected: Some(expected),
                        actual: Some(actual),
                        size_changed,
                        mtime_changed,
                    });
                } else {
                    failed += 1;
                    items.push(ControlledVerifyItem {
                        rel_path: row.rel_path,
                        status: "fail".into(),
                        message: msg,
                        expected: Some(expected),
                        actual: Some(actual),
                        size_changed,
                        mtime_changed,
                    });
                }
            }
            Err(e) => {
                errors += 1;
                items.push(ControlledVerifyItem {
                    rel_path: row.rel_path,
                    status: "error".into(),
                    message: e,
                    expected: None,
                    actual: None,
                    size_changed,
                    mtime_changed,
                });
            }
        }
    }

    Ok(ControlledVerifyReport {
        drive_root: drive_root.to_string_lossy().to_string(),
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

/// Resolve user-selected absolute paths (files and/or directories) to controlled
/// files already recorded in vault.db. Uncontrolled paths are skipped silently
/// (they never appear in the returned list or verify results).
pub fn resolve_controlled_selection(
    drive_root: &Path,
    paths: &[String],
) -> Result<Vec<ControlledFile>, String> {
    if !disk::is_backup_disk(drive_root) {
        return Err("当前盘不是 DataVault 备份盘".into());
    }
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let all = list_controlled_files(drive_root)?;
    if all.is_empty() {
        return Ok(Vec::new());
    }

    let root_s = drive_root
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\");

    let mut exact: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut prefixes: Vec<String> = Vec::new();
    let mut select_all = false;

    for p in paths {
        let abs = PathBuf::from(p);
        let abs_s = abs
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .replace('/', "\\");
        if abs_s.eq_ignore_ascii_case(&root_s) {
            select_all = true;
            break;
        }
        if abs.is_dir() {
            match normalize_rel_path(drive_root, &abs) {
                Ok(rel) => {
                    let base = rel.trim_end_matches(['\\', '/']).to_string();
                    prefixes.push(format!("{base}\\"));
                    exact.insert(base);
                }
                Err(_) => {}
            }
        } else if abs.is_file() {
            if let Ok(rel) = normalize_rel_path(drive_root, &abs) {
                exact.insert(rel);
            }
        } else if let Ok(rel) = normalize_rel_path(drive_root, &abs) {
            let base = rel.trim_end_matches(['\\', '/']).to_string();
            exact.insert(base.clone());
            prefixes.push(format!("{base}\\"));
        }
    }

    if select_all {
        return Ok(all);
    }

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in all {
        let rel_l = f.rel_path.to_lowercase();
        let hit = exact.iter().any(|e| e.eq_ignore_ascii_case(&f.rel_path))
            || prefixes.iter().any(|pre| rel_l.starts_with(&pre.to_lowercase()));
        if hit && seen.insert(rel_l) {
            out.push(f);
        }
    }
    Ok(out)
}

