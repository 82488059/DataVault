//! Full MD5 and FastMD5 (chunked sampling) for controlled files.

use md5::{Digest, Md5};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const DEFAULT_SAMPLE_RATIO: f64 = 0.10;
pub const DEFAULT_SAMPLE_CHUNK_MB: i64 = 100;

pub fn compute_md5_full(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("打开失败 {}: {e}", path.display()))?;
    let mut hasher = Md5::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// FastMD5: for each chunk of `sample_chunk_mb` MiB (incl. remainder),
/// hash the first `floor(block_len * sample_ratio)` bytes into one MD5 context.
pub fn compute_fast_md5(
    path: &Path,
    sample_ratio: f64,
    sample_chunk_mb: i64,
) -> Result<String, String> {
    if !(sample_ratio > 0.0 && sample_ratio <= 1.0) {
        return Err(format!("无效 sample_ratio: {sample_ratio}"));
    }
    if sample_chunk_mb <= 0 {
        return Err(format!("无效 sample_chunk_mb: {sample_chunk_mb}"));
    }
    let meta = fs::metadata(path).map_err(|e| format!("元数据失败 {}: {e}", path.display()))?;
    let size = meta.len();
    let chunk: u64 = (sample_chunk_mb as u64)
        .checked_mul(1024 * 1024)
        .ok_or_else(|| "sample_chunk_mb 过大".to_string())?;

    let mut hasher = Md5::new();
    if size == 0 {
        return Ok(format!("{:x}", hasher.finalize()));
    }

    let mut file = File::open(path).map_err(|e| format!("打开失败 {}: {e}", path.display()))?;
    let mut offset: u64 = 0;
    let mut buf = Vec::new();

    while offset < size {
        let block_len = std::cmp::min(chunk, size - offset);
        let sample_len = ((block_len as f64) * sample_ratio).floor() as u64;
        if sample_len > 0 {
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| format!("seek 失败: {e}"))?;
            buf.resize(sample_len as usize, 0);
            let mut read_total = 0usize;
            while read_total < buf.len() {
                match file.read(&mut buf[read_total..]) {
                    Ok(0) => break,
                    Ok(n) => read_total += n,
                    Err(e) => return Err(format!("读取失败: {e}")),
                }
            }
            hasher.update(&buf[..read_total]);
        }
        offset = offset.saturating_add(block_len);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "dv-{tag}-{}.bin",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn empty_file_hashes() {
        let p = temp_path("empty");
        File::create(&p).unwrap();
        let full = compute_md5_full(&p).unwrap();
        let fast = compute_fast_md5(&p, 0.10, 100).unwrap();
        assert_eq!(full, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(fast, "d41d8cd98f00b204e9800998ecf8427e");
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn fast_md5_samples_first_10_percent_of_chunk() {
        let p = temp_path("fast");
        let mut f = File::create(&p).unwrap();
        let data: Vec<u8> = (0u8..250).collect();
        f.write_all(&data).unwrap();
        drop(f);

        assert!(compute_fast_md5(&p, 0.10, 0).is_err());

        // chunk_mb=1 => 1 MiB > 250, single remainder chunk, sample floor(250*0.1)=25
        let fast = compute_fast_md5(&p, 0.10, 1).unwrap();
        let mut h = Md5::new();
        h.update(&data[..25]);
        let expect = format!("{:x}", h.finalize());
        assert_eq!(fast, expect);
        let _ = fs::remove_file(&p);
    }
}
