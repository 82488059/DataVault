//! Backup-disk marker: `<drive_root>/.datavault/disk.json`

use chrono::{FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "DataVault";
pub const META_DIR: &str = ".datavault";
pub const DISK_JSON: &str = "disk.json";
pub const VAULT_DB: &str = "vault.db";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DiskJson {
    pub is_backup_disk: bool,
    pub app: String,
    pub created_at: String,
    pub updated_at: String,
}

pub fn shanghai_now_iso() -> String {
    let offset = FixedOffset::east_opt(8 * 3600).expect("UTC+8");
    Utc::now().with_timezone(&offset).to_rfc3339()
}

pub fn meta_dir(drive_root: &Path) -> PathBuf {
    drive_root.join(META_DIR)
}

pub fn disk_json_path(drive_root: &Path) -> PathBuf {
    meta_dir(drive_root).join(DISK_JSON)
}

pub fn vault_db_path(drive_root: &Path) -> PathBuf {
    meta_dir(drive_root).join(VAULT_DB)
}

/// Normalize drive root to `X:\` form when the input looks like a drive letter.
pub fn normalize_drive_root(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    let root = if chars.len() >= 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic() {
        let letter = chars[0].to_ascii_uppercase();
        PathBuf::from(format!(r"{letter}:\"))
    } else {
        return Err(format!("无法识别盘符根路径: {trimmed}"));
    };
    if !root.exists() {
        return Err(format!("盘符不存在或不可访问: {}", root.display()));
    }
    Ok(root)
}

pub fn read_disk_json(drive_root: &Path) -> Result<Option<DiskJson>, String> {
    let path = disk_json_path(drive_root);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("读取 disk.json 失败: {e}"))?;
    let parsed: DiskJson =
        serde_json::from_str(&text).map_err(|e| format!("解析 disk.json 失败: {e}"))?;
    Ok(Some(parsed))
}

pub fn is_backup_disk(drive_root: &Path) -> bool {
    match read_disk_json(drive_root) {
        Ok(Some(j)) => j.is_backup_disk && j.app == APP_NAME,
        _ => false,
    }
}


/// Extract `X:\` from a path like `X:\foo\bar` (Windows drive letter).
pub fn drive_root_of(path: &Path) -> Option<PathBuf> {
    let s = path.to_string_lossy();
    let mut chars = s.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    if chars.next() != Some(':') {
        return None;
    }
    Some(PathBuf::from(format!(r"{}:\", letter.to_ascii_uppercase())))
}

/// Create/refresh `.datavault/disk.json` and ensure `vault.db` exists.
pub fn mark_backup_disk(drive_root: &Path) -> Result<DiskJson, String> {
    let meta = meta_dir(drive_root);
    fs::create_dir_all(&meta).map_err(|e| {
        format!(
            "无法创建 {}（可能无写权限或介质只读）: {e}",
            meta.display()
        )
    })?;

    let now = shanghai_now_iso();
    let json_path = disk_json_path(drive_root);

    let disk = if json_path.exists() {
        let existing = read_disk_json(drive_root)?.ok_or_else(|| "disk.json 丢失".to_string())?;
        if existing.app != APP_NAME {
            return Err(format!(
                "目录已被其他应用占用（app={}），拒绝标记",
                existing.app
            ));
        }
        DiskJson {
            is_backup_disk: true,
            app: APP_NAME.to_string(),
            created_at: existing.created_at,
            updated_at: now,
        }
    } else {
        DiskJson {
            is_backup_disk: true,
            app: APP_NAME.to_string(),
            created_at: now.clone(),
            updated_at: now,
        }
    };

    let body = serde_json::to_string_pretty(&disk).map_err(|e| format!("序列化失败: {e}"))?;
    fs::write(&json_path, body.as_bytes()).map_err(|e| format!("写入 disk.json 失败: {e}"))?;

    crate::vault::ensure_db(drive_root)?;
    Ok(disk)
}
