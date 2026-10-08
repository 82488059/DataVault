//! Pack selected files into a ustar .tar and list controlled archive entries.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TarEntryInfo {
    pub name: String,
    pub tar_path: String,
    pub rel_path: String,
    pub is_dir: bool,
    pub size: u64,
}

fn norm_slash(s: &str) -> String {
    s.replace('\\', "/")
}

fn trim_slashes(s: &str) -> &str {
    s.trim_matches('/')
}

pub fn list_entries(tar_path: &Path, prefix: &str) -> Result<Vec<TarEntryInfo>, String> {
    if !tar_path.is_file() {
        return Err(format!("不是文件: {}", tar_path.display()));
    }
    let name_l = tar_path
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !name_l.ends_with(".tar") {
        return Err("仅支持 .tar 归档".into());
    }

    let prefix = trim_slashes(&norm_slash(prefix)).to_string();
    let prefix_with = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };

    let file = File::open(tar_path).map_err(|e| format!("打开 tar 失败: {e}"))?;
    let mut archive = tar::Archive::new(file);
    let mut dirs: std::collections::BTreeMap<String, TarEntryInfo> =
        std::collections::BTreeMap::new();
    let mut files: std::collections::BTreeMap<String, TarEntryInfo> =
        std::collections::BTreeMap::new();

    let entries = archive
        .entries()
        .map_err(|e| format!("读取 tar 失败: {e}"))?;
    for ent in entries {
        let ent = ent.map_err(|e| format!("tar 条目错误: {e}"))?;
        let path = ent
            .path()
            .map_err(|e| format!("tar 路径错误: {e}"))?
            .to_string_lossy()
            .replace('\\', "/");
        let path = trim_slashes(&path).to_string();
        if path.is_empty() {
            continue;
        }
        let rest = if prefix_with.is_empty() {
            path.clone()
        } else if path.starts_with(&prefix_with) {
            path[prefix_with.len()..].to_string()
        } else if path == prefix {
            continue;
        } else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let (child, has_deeper) = match rest.split_once('/') {
            Some((c, rest2)) => (c.to_string(), !rest2.is_empty()),
            None => (rest.clone(), false),
        };
        if child.is_empty() || child == "." || child == ".." {
            continue;
        }
        let child_rel = if prefix.is_empty() {
            child.clone()
        } else {
            format!("{prefix}/{child}")
        };
        let is_dir =
            has_deeper || ent.header().entry_type().is_dir() || rest.contains('/');
        let size = if is_dir {
            0
        } else {
            ent.header().size().unwrap_or(0)
        };
        let info = TarEntryInfo {
            name: child.clone(),
            tar_path: tar_path.to_string_lossy().to_string(),
            rel_path: if is_dir {
                format!("{child_rel}/")
            } else {
                child_rel.clone()
            },
            is_dir,
            size,
        };
        if is_dir {
            dirs.entry(child.to_lowercase()).or_insert(info);
        } else {
            files
                .entry(child.to_lowercase())
                .and_modify(|e| {
                    if e.size == 0 && size > 0 {
                        *e = info.clone();
                    }
                })
                .or_insert(info);
        }
    }

    for k in dirs.keys() {
        files.remove(k);
    }
    let mut out: Vec<TarEntryInfo> = dirs.into_values().chain(files.into_values()).collect();
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(out)
}

pub fn pack_files(
    tar_path: &Path,
    collected: &[(PathBuf, PathBuf)],
    mut on_progress: impl FnMut(usize, &str),
) -> Result<u64, String> {
    if let Some(parent) = tar_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let file = File::create(tar_path).map_err(|e| format!("创建 tar 失败: {e}"))?;
    let mut builder = tar::Builder::new(file);
    builder.mode(tar::HeaderMode::Deterministic);

    for (i, (abs, rel)) in collected.iter().enumerate() {
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        on_progress(i + 1, &rel_s);
        if !abs.is_file() {
            return Err(format!("不是文件: {}", abs.display()));
        }
        let mut f = File::open(abs).map_err(|e| format!("打开源文件失败 {}: {e}", abs.display()))?;
        let meta = f.metadata().map_err(|e| format!("元数据失败: {e}"))?;
        let mut header = tar::Header::new_gnu();
        header.set_metadata_in_mode(&meta, tar::HeaderMode::Deterministic);
        header
            .set_path(&rel_s)
            .map_err(|e| format!("设置 tar 路径失败: {e}"))?;
        header.set_size(meta.len());
        header.set_cksum();
        builder
            .append_data(&mut header, &rel_s, &mut f)
            .map_err(|e| format!("写入 tar 失败 {rel_s}: {e}"))?;
    }
    builder
        .finish()
        .map_err(|e| format!("完成 tar 失败: {e}"))?;
    Ok(std::fs::metadata(tar_path)
        .map(|m| m.len())
        .unwrap_or(0))
}

pub fn name_matches(file_name: &str, re: &regex::Regex, exclude: bool) -> bool {
    let m = re.is_match(file_name);
    if exclude { !m } else { m }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn pack_streams_file_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "dv-tar-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("hello.txt");
        std::fs::write(&src, b"hello-tar-stream").unwrap();
        let tar_path = dir.join("out.tar");
        let rel = PathBuf::from("sub").join("hello.txt");
        let n = pack_files(&tar_path, &[(src, rel)], |_, _| {}).unwrap();
        assert!(n > 0);
        let mut archive = tar::Archive::new(File::open(&tar_path).unwrap());
        let mut found = false;
        for ent in archive.entries().unwrap() {
            let mut ent = ent.unwrap();
            let path = ent.path().unwrap().to_string_lossy().replace('\\', "/");
            if path == "sub/hello.txt" {
                let mut body = Vec::new();
                ent.read_to_end(&mut body).unwrap();
                assert_eq!(body, b"hello-tar-stream");
                found = true;
            }
        }
        assert!(found);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
