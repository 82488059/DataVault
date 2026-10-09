//! Export / verify / restore of drive-root `.datavault` as a zip + manifest.

use crate::disk::{self, APP_NAME, META_DIR};
use chrono::{FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub const MANIFEST_NAME: &str = "manifest.json";
pub const FORMAT_VERSION: u32 = 1;
pub const HASH_ALG: &str = "sha256";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ManifestEntry {
    pub path: String,
    pub size: u64,
    pub hash: String,
    pub hash_alg: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    pub format_version: u32,
    pub app: String,
    pub exported_at: String,
    pub source_drive: String,
    pub entries: Vec<ManifestEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyReport {
    pub ok: bool,
    pub message: String,
    pub entry_count: usize,
    pub source_drive: Option<String>,
    pub exported_at: Option<String>,
    pub failures: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExportResult {
    pub zip_path: String,
    pub entry_count: usize,
    pub source_drive: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RestoreResult {
    pub drive_root: String,
    pub backup_path: Option<String>,
    pub entry_count: usize,
    pub message: String,
}

fn shanghai_now_iso() -> String {
    let offset = FixedOffset::east_opt(8 * 3600).expect("UTC+8");
    Utc::now().with_timezone(&offset).to_rfc3339()
}

fn stamp_compact() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

fn drive_letter_label(root: &Path) -> String {
    root.to_string_lossy()
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "X".into())
}

/// Default zip file name: `DataVault-meta-<letter>-YYYYMMDDHHmmss.zip`
pub fn default_export_name(drive: &str) -> Result<String, String> {
    let root = disk::normalize_drive_root(drive)?;
    let letter = drive_letter_label(&root);
    Ok(format!("DataVault-meta-{letter}-{}.zip", stamp_compact()))
}

fn sha256_file(path: &Path) -> Result<(u64, String), String> {
    let mut file = File::open(path).map_err(|e| format!("打开失败 {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

fn sha256_reader<R: Read>(mut reader: R) -> Result<(u64, String), String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut size = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

fn collect_meta_files(meta: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let rd = fs::read_dir(meta).map_err(|e| format!("读取 {} 失败: {e}", meta.display()))?;
    for ent in rd {
        let ent = ent.map_err(|e| format!("读取目录项失败: {e}"))?;
        let path = ent.path();
        let ft = ent.file_type().map_err(|e| format!("文件类型失败: {e}"))?;
        if ft.is_file() {
            out.push(path);
        } else if ft.is_dir() {
            collect_files_recursive(&path, &mut out)?;
        }
    }
    out.sort();
    Ok(out)
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("读取 {} 失败: {e}", dir.display()))?;
    for ent in rd {
        let ent = ent.map_err(|e| format!("读取目录项失败: {e}"))?;
        let path = ent.path();
        let ft = ent.file_type().map_err(|e| format!("文件类型失败: {e}"))?;
        if ft.is_file() {
            out.push(path);
        } else if ft.is_dir() {
            collect_files_recursive(&path, out)?;
        }
    }
    Ok(())
}

fn archive_rel_path(meta: &Path, file: &Path) -> Result<String, String> {
    let rel = file
        .strip_prefix(meta)
        .map_err(|_| format!("路径不在 .datavault 内: {}", file.display()))?;
    let mut parts = vec![META_DIR.to_string()];
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().to_string()),
            Component::CurDir => {}
            _ => return Err(format!("非法相对路径: {}", file.display())),
        }
    }
    Ok(parts.join("/"))
}

fn ensure_zip_path(zip_path: &str) -> Result<PathBuf, String> {
    let mut p = PathBuf::from(zip_path.trim());
    if p.as_os_str().is_empty() {
        return Err("请指定 zip 保存路径".into());
    }
    if p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("zip")) != Some(true)
    {
        p.set_extension("zip");
    }
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建目录失败 {}: {e}", parent.display()))?;
        }
    }
    Ok(p)
}

pub fn export_datavault_metadata(drive: &str, zip_path: &str) -> Result<ExportResult, String> {
    let root = disk::normalize_drive_root(drive)?;
    if !disk::is_backup_disk(&root) {
        return Err("当前盘不是 DataVault 受控盘，请先标记".into());
    }
    let meta = disk::meta_dir(&root);
    if !meta.is_dir() {
        return Err(format!("未找到元数据目录: {}", meta.display()));
    }

    let dest = ensure_zip_path(zip_path)?;
    if dest.exists() {
        return Err(format!("目标已存在，请换文件名: {}", dest.display()));
    }

    let files = collect_meta_files(&meta)?;
    if files.is_empty() {
        return Err(".datavault 目录为空，无法导出".into());
    }

    let mut entries: Vec<ManifestEntry> = Vec::with_capacity(files.len());
    for f in &files {
        let (size, hash) = sha256_file(f)?;
        let path = archive_rel_path(&meta, f)?;
        entries.push(ManifestEntry {
            path,
            size,
            hash,
            hash_alg: HASH_ALG.into(),
        });
    }

    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        app: APP_NAME.into(),
        exported_at: shanghai_now_iso(),
        source_drive: format!("{}:", drive_letter_label(&root)),
        entries: entries.clone(),
    };
    let manifest_json =
        serde_json::to_string_pretty(&manifest).map_err(|e| format!("序列化 manifest 失败: {e}"))?;

    let tmp = dest.with_extension("zip.partial");
    let _ = fs::remove_file(&tmp);
    let file = File::create(&tmp).map_err(|e| format!("创建 zip 失败: {e}"))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file(MANIFEST_NAME, opts)
        .map_err(|e| format!("写入 manifest 条目失败: {e}"))?;
    zip.write_all(manifest_json.as_bytes())
        .map_err(|e| format!("写入 manifest 内容失败: {e}"))?;

    for (f, ent) in files.iter().zip(entries.iter()) {
        zip.start_file(&ent.path, opts)
            .map_err(|e| format!("写入条目失败 {}: {e}", ent.path))?;
        let mut src = File::open(f).map_err(|e| format!("打开 {} 失败: {e}", f.display()))?;
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = src.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n])
                .map_err(|e| format!("写入 zip 失败: {e}"))?;
        }
    }

    zip.finish().map_err(|e| format!("完成 zip 失败: {e}"))?;
    fs::rename(&tmp, &dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("保存 zip 失败: {e}")
    })?;

    Ok(ExportResult {
        zip_path: dest.to_string_lossy().to_string(),
        entry_count: entries.len(),
        source_drive: format!("{}:", drive_letter_label(&root)),
    })
}

fn open_zip(zip_path: &Path) -> Result<ZipArchive<File>, String> {
    let file =
        File::open(zip_path).map_err(|e| format!("打开 zip 失败 {}: {e}", zip_path.display()))?;
    ZipArchive::new(file).map_err(|e| format!("解析 zip 失败: {e}"))
}

fn read_manifest(archive: &mut ZipArchive<File>) -> Result<Manifest, String> {
    let mut file = archive
        .by_name(MANIFEST_NAME)
        .map_err(|_| "zip 中缺少 manifest.json".to_string())?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| format!("读取 manifest 失败: {e}"))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("解析 manifest 失败: {e}"))?;
    if manifest.format_version != FORMAT_VERSION {
        return Err(format!(
            "不支持的 manifest 版本: {}（当前支持 {FORMAT_VERSION}）",
            manifest.format_version
        ));
    }
    if manifest.app != APP_NAME {
        return Err(format!("非 DataVault 元数据归档（app={}）", manifest.app));
    }
    Ok(manifest)
}

fn normalize_zip_name(name: &str) -> String {
    name.replace('\\', "/")
}

pub fn verify_datavault_backup_zip(zip_path: &str) -> Result<VerifyReport, String> {
    let path = PathBuf::from(zip_path.trim());
    if !path.is_file() {
        return Err(format!("找不到 zip 文件: {}", path.display()));
    }
    let mut archive = open_zip(&path)?;
    let manifest = match read_manifest(&mut archive) {
        Ok(m) => m,
        Err(e) => {
            return Ok(VerifyReport {
                ok: false,
                message: e,
                entry_count: 0,
                source_drive: None,
                exported_at: None,
                failures: vec![],
            });
        }
    };

    let mut failures: Vec<String> = Vec::new();
    let mut listed: std::collections::HashSet<String> = std::collections::HashSet::new();

    for ent in &manifest.entries {
        listed.insert(normalize_zip_name(&ent.path));
        let norm = normalize_zip_name(&ent.path);
        let mut zf = match archive.by_name(&norm) {
            Ok(f) => f,
            Err(_) => {
                failures.push(format!("缺失: {}", ent.path));
                continue;
            }
        };
        let (size, hash) = match sha256_reader(&mut zf) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("读取失败 {}: {e}", ent.path));
                continue;
            }
        };
        if size != ent.size {
            failures.push(format!(
                "大小不符 {}: 期望 {}，实际 {}",
                ent.path, ent.size, size
            ));
        }
        let expect_alg = if ent.hash_alg.is_empty() {
            HASH_ALG
        } else {
            ent.hash_alg.as_str()
        };
        if expect_alg != HASH_ALG {
            failures.push(format!("不支持的哈希算法 {}: {}", ent.path, expect_alg));
        } else if hash != ent.hash.to_lowercase() {
            failures.push(format!("哈希不符: {}", ent.path));
        }
    }

    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| normalize_zip_name(f.name())))
        .collect();
    for name in names {
        if name.ends_with('/') {
            continue;
        }
        if name == MANIFEST_NAME {
            continue;
        }
        if !listed.contains(&name) {
            failures.push(format!("未列入清单的多余文件: {name}"));
        }
    }

    let disk_json_name = format!("{META_DIR}/disk.json");
    if let Ok(mut zf) = archive.by_name(&disk_json_name) {
        let mut text = String::new();
        if zf.read_to_string(&mut text).is_ok() {
            if let Ok(dj) = serde_json::from_str::<disk::DiskJson>(&text) {
                if dj.app != APP_NAME || !dj.is_backup_disk {
                    failures.push("disk.json 语义校验失败：非 DataVault 受控盘标记".into());
                }
            } else {
                failures.push("disk.json 无法解析".into());
            }
        }
    } else {
        failures.push("缺少 .datavault/disk.json".into());
    }

    let ok = failures.is_empty();
    Ok(VerifyReport {
        ok,
        message: if ok {
            format!(
                "校验通过：{} 个文件，源盘 {}，导出于 {}",
                manifest.entries.len(),
                manifest.source_drive,
                manifest.exported_at
            )
        } else {
            format!("校验失败（{} 项）", failures.len())
        },
        entry_count: manifest.entries.len(),
        source_drive: Some(manifest.source_drive),
        exported_at: Some(manifest.exported_at),
        failures,
    })
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("创建备份目录失败: {e}"))?;
    for ent in fs::read_dir(src).map_err(|e| format!("读取源目录失败: {e}"))? {
        let ent = ent.map_err(|e| format!("读取目录项失败: {e}"))?;
        let from = ent.path();
        let to = dest.join(ent.file_name());
        let ft = ent.file_type().map_err(|e| format!("文件类型失败: {e}"))?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            fs::copy(&from, &to).map_err(|e| format!("复制 {} 失败: {e}", from.display()))?;
        }
    }
    Ok(())
}

fn remove_dir_all_best_effort(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

pub fn restore_datavault_metadata(drive: &str, zip_path: &str) -> Result<RestoreResult, String> {
    let root = disk::normalize_drive_root(drive)?;
    let meta = disk::meta_dir(&root);

    let report = verify_datavault_backup_zip(zip_path)?;
    if !report.ok {
        let detail = if report.failures.is_empty() {
            report.message
        } else {
            format!("{}：{}", report.message, report.failures.join("；"))
        };
        return Err(format!("恢复前校验失败，未改动现有目录。{detail}"));
    }

    let path = PathBuf::from(zip_path.trim());
    let mut archive = open_zip(&path)?;
    let manifest = read_manifest(&mut archive)?;

    let staging = root.join(format!(".datavault.restore-tmp-{}", stamp_compact()));
    if staging.exists() {
        remove_dir_all_best_effort(&staging);
    }
    let staging_meta = staging.join(META_DIR);
    fs::create_dir_all(&staging_meta).map_err(|e| format!("创建临时目录失败: {e}"))?;

    let extract_result = (|| -> Result<(), String> {
        for ent in &manifest.entries {
            let name = normalize_zip_name(&ent.path);
            let prefix = format!("{META_DIR}/");
            if !name.starts_with(&prefix) {
                return Err(format!("非法归档路径（须以 .datavault/ 开头）: {name}"));
            }
            let rel = &name[prefix.len()..];
            if rel.is_empty() || rel.contains("..") {
                return Err(format!("非法相对路径: {name}"));
            }
            let out_path = staging_meta.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("创建子目录失败: {e}"))?;
            }
            let mut zf = archive
                .by_name(&name)
                .map_err(|_| format!("zip 中缺少 {}", ent.path))?;
            let mut out = File::create(&out_path)
                .map_err(|e| format!("写入临时文件失败 {}: {e}", out_path.display()))?;
            let mut buf = vec![0u8; 1024 * 1024];
            loop {
                let n = zf.read(&mut buf).map_err(|e| format!("解压读取失败: {e}"))?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])
                    .map_err(|e| format!("解压写入失败: {e}"))?;
            }
        }
        let dj_path = staging_meta.join("disk.json");
        let text =
            fs::read_to_string(&dj_path).map_err(|e| format!("读取临时 disk.json 失败: {e}"))?;
        let dj: disk::DiskJson =
            serde_json::from_str(&text).map_err(|e| format!("解析临时 disk.json 失败: {e}"))?;
        if dj.app != APP_NAME || !dj.is_backup_disk {
            return Err("非 DataVault 元数据归档".into());
        }
        Ok(())
    })();

    if let Err(e) = extract_result {
        remove_dir_all_best_effort(&staging);
        return Err(e);
    }

    let mut backup_path: Option<String> = None;
    if meta.exists() {
        let bak = root.join(format!(".datavault.bak-{}", stamp_compact()));
        if let Err(e) = copy_dir_recursive(&meta, &bak) {
            remove_dir_all_best_effort(&staging);
            let _ = fs::remove_dir_all(&bak);
            return Err(format!("备份现有 .datavault 失败，已中止恢复: {e}"));
        }
        backup_path = Some(bak.to_string_lossy().to_string());
        if let Err(e) = fs::remove_dir_all(&meta) {
            remove_dir_all_best_effort(&staging);
            return Err(format!(
                "无法移除旧 .datavault（已保留备份 {:?}）: {e}",
                backup_path
            ));
        }
    }

    if let Err(e) = fs::rename(&staging_meta, &meta) {
        if let Err(e2) = copy_dir_recursive(&staging_meta, &meta) {
            remove_dir_all_best_effort(&staging);
            return Err(format!(
                "安装新 .datavault 失败: {e}; 回退复制也失败: {e2}；旧备份={:?}",
                backup_path
            ));
        }
    }
    remove_dir_all_best_effort(&staging);

    let _ = crate::vault::ensure_db(&root);

    Ok(RestoreResult {
        drive_root: root.to_string_lossy().to_string(),
        backup_path: backup_path.clone(),
        entry_count: manifest.entries.len(),
        message: match &backup_path {
            Some(b) => format!(
                "已恢复到 {}（{} 个文件）；原目录备份于 {}",
                meta.display(),
                manifest.entries.len(),
                b
            ),
            None => format!(
                "已恢复到 {}（{} 个文件）",
                meta.display(),
                manifest.entries.len()
            ),
        },
    })
}
