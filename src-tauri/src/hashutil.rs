//! Full MD5 and FastMD5 (chunked sampling) for controlled files.

use md5::{Digest, Md5};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const DEFAULT_SAMPLE_RATIO: f64 = 0.10;
pub const DEFAULT_SAMPLE_CHUNK_MB: i64 = 100;
pub const READ_BUF_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Md5Pair {
    pub md5: String,
    pub fast_md5: String,
}

pub fn compute_md5_full(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("打开失败 {}: {e}", path.display()))?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; READ_BUF_SIZE];
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
    let mut buf = vec![0u8; READ_BUF_SIZE];

    while offset < size {
        let block_len = std::cmp::min(chunk, size - offset);
        let sample_len = ((block_len as f64) * sample_ratio).floor() as u64;
        if sample_len > 0 {
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| format!("seek 失败: {e}"))?;
            let mut left = sample_len;
            while left > 0 {
                let want = std::cmp::min(buf.len() as u64, left) as usize;
                match file.read(&mut buf[..want]) {
                    Ok(0) => break,
                    Ok(n) => {
                        hasher.update(&buf[..n]);
                        left -= n as u64;
                    }
                    Err(e) => return Err(format!("读取失败: {e}")),
                }
            }
        }
        offset = offset.saturating_add(block_len);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// One sequential read: full MD5 over every byte, FastMD5 over each chunk's sample prefix.
pub fn compute_md5_pair(
    path: &Path,
    sample_ratio: f64,
    sample_chunk_mb: i64,
) -> Result<Md5Pair, String> {
    let mut buf = vec![0u8; READ_BUF_SIZE];
    compute_md5_pair_buf(path, sample_ratio, sample_chunk_mb, &mut buf)
}

/// Same as [`compute_md5_pair`], reusing `buf` across files (resized up to [`READ_BUF_SIZE`]).
pub fn compute_md5_pair_buf(
    path: &Path,
    sample_ratio: f64,
    sample_chunk_mb: i64,
    buf: &mut Vec<u8>,
) -> Result<Md5Pair, String> {
    if !(sample_ratio > 0.0 && sample_ratio <= 1.0) {
        return Err(format!("无效 sample_ratio: {sample_ratio}"));
    }
    let chunk = chunk_bytes(sample_chunk_mb)?;
    if buf.len() < READ_BUF_SIZE {
        buf.resize(READ_BUF_SIZE, 0);
    }
    hash_pair_with(path, sample_ratio, chunk, &mut buf[..READ_BUF_SIZE])
}

fn chunk_bytes(sample_chunk_mb: i64) -> Result<u64, String> {
    if sample_chunk_mb <= 0 {
        return Err(format!("无效 sample_chunk_mb: {sample_chunk_mb}"));
    }
    (sample_chunk_mb as u64)
        .checked_mul(1024 * 1024)
        .ok_or_else(|| "sample_chunk_mb 过大".to_string())
}

fn hash_pair_with(path: &Path, sample_ratio: f64, chunk: u64, buf: &mut [u8]) -> Result<Md5Pair, String> {
    if !(sample_ratio > 0.0 && sample_ratio <= 1.0) {
        return Err(format!("无效 sample_ratio: {sample_ratio}"));
    }
    if chunk == 0 {
        return Err("无效采样块大小".into());
    }
    if buf.is_empty() {
        return Err("读缓冲为空".into());
    }
    let meta = fs::metadata(path).map_err(|e| format!("元数据失败 {}: {e}", path.display()))?;
    let size = meta.len();
    let mut full = Md5::new();
    let mut fast = Md5::new();
    if size == 0 {
        return Ok(Md5Pair {
            md5: format!("{:x}", full.finalize()),
            fast_md5: format!("{:x}", fast.finalize()),
        });
    }
    let mut file = File::open(path).map_err(|e| format!("打开失败 {}: {e}", path.display()))?;
    let mut pos = 0u64;
    while pos < size {
        let want = std::cmp::min(buf.len() as u64, size - pos) as usize;
        let n = file
            .read(&mut buf[..want])
            .map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        full.update(&buf[..n]);
        feed_fast(&mut fast, &buf[..n], pos, size, chunk, sample_ratio);
        pos += n as u64;
    }
    Ok(Md5Pair {
        md5: format!("{:x}", full.finalize()),
        fast_md5: format!("{:x}", fast.finalize()),
    })
}

fn feed_fast(hasher: &mut Md5, data: &[u8], pos: u64, file_size: u64, chunk: u64, sample_ratio: f64) {
    let end = pos + data.len() as u64;
    let mut cursor = pos;
    while cursor < end {
        let chunk_start = (cursor / chunk) * chunk;
        let block_len = std::cmp::min(chunk, file_size.saturating_sub(chunk_start));
        if block_len == 0 {
            break;
        }
        let sample_len = ((block_len as f64) * sample_ratio).floor() as u64;
        let sample_end = chunk_start + sample_len;
        let chunk_end = chunk_start + block_len;
        if cursor >= sample_end {
            cursor = chunk_end;
            continue;
        }
        let take_end = std::cmp::min(sample_end, end);
        let start_i = (cursor - pos) as usize;
        let end_i = (take_end - pos) as usize;
        hasher.update(&data[start_i..end_i]);
        cursor = take_end;
    }
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

    #[test]
    fn pair_matches_full_and_fast_across_small_reads() {
        let p = temp_path("pair");
        let mut data = vec![0u8; 1000];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        fs::write(&p, &data).unwrap();

        let mut small = vec![0u8; 64];
        let pair = super::hash_pair_with(&p, 0.10, 300, &mut small).unwrap();
        let mut full = Md5::new();
        full.update(&data);
        assert_eq!(pair.md5, format!("{:x}", full.finalize()));

        let mut fast = Md5::new();
        let mut offset = 0u64;
        let size = data.len() as u64;
        let chunk = 300u64;
        while offset < size {
            let block_len = std::cmp::min(chunk, size - offset);
            let sample_len = ((block_len as f64) * 0.10).floor() as u64;
            if sample_len > 0 {
                let start = offset as usize;
                fast.update(&data[start..start + sample_len as usize]);
            }
            offset += block_len;
        }
        assert_eq!(pair.fast_md5, format!("{:x}", fast.finalize()));

        let via_api = compute_md5_pair(&p, 0.10, 1).unwrap();
        assert_eq!(via_api.md5, compute_md5_full(&p).unwrap());
        assert_eq!(via_api.fast_md5, compute_fast_md5(&p, 0.10, 1).unwrap());
        let _ = fs::remove_file(&p);
    }
}
