use anyhow::{Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PackedEntry {
    pub pack_path: PathBuf,
    pub byte_offset: u64,
    pub compressed_size: u64,
    pub original_size: u64,
    pub sha256: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActiveChunk {
    pub entry_id: u64,
    pub byte_offset: u64,
    pub compressed_size: u64,
    pub new_byte_offset: u64,
}

/// Compresses `file_paths` in parallel across all CPU cores using Zstandard (level 3)
/// and appends them as independent seekable frames into `pack_path`.
pub fn append_batch(pack_path: &Path, file_paths: &[PathBuf]) -> Result<Vec<PackedEntry>> {
    if file_paths.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(parent) = pack_path.parent() {
        fs::create_dir_all(parent)?;
    }

    struct CompressedJob {
        original_size: u64,
        sha256_hex: String,
        compressed: Vec<u8>,
        error: Option<String>,
    }

    let jobs: Vec<CompressedJob> = file_paths
        .par_iter()
        .map(|p| match fs::read(p) {
            Ok(raw) => {
                let mut hasher = Sha256::new();
                hasher.update(&raw);
                let sha256_hex = hex::encode(hasher.finalize());
                match zstd::encode_all(raw.as_slice(), 3) {
                    Ok(compressed) => CompressedJob {
                        original_size: raw.len() as u64,
                        sha256_hex,
                        compressed,
                        error: None,
                    },
                    Err(e) => CompressedJob {
                        original_size: 0,
                        sha256_hex: String::new(),
                        compressed: Vec::new(),
                        error: Some(e.to_string()),
                    },
                }
            }
            Err(e) => CompressedJob {
                original_size: 0,
                sha256_hex: String::new(),
                compressed: Vec::new(),
                error: Some(e.to_string()),
            },
        })
        .collect();

    let mut pack_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(pack_path)
        .with_context(|| format!("failed to open pack file {}", pack_path.display()))?;

    let mut offset = pack_file.seek(SeekFrom::End(0))?;
    let mut entries = Vec::with_capacity(jobs.len());

    for job in jobs {
        if let Some(err) = job.error {
            entries.push(PackedEntry {
                pack_path: pack_path.to_path_buf(),
                byte_offset: 0,
                compressed_size: 0,
                original_size: 0,
                sha256: String::new(),
                error: Some(err),
            });
            continue;
        }
        let comp_len = job.compressed.len() as u64;
        if let Err(e) = pack_file.write_all(&job.compressed) {
            entries.push(PackedEntry {
                pack_path: pack_path.to_path_buf(),
                byte_offset: 0,
                compressed_size: 0,
                original_size: 0,
                sha256: String::new(),
                error: Some(e.to_string()),
            });
            continue;
        }
        entries.push(PackedEntry {
            pack_path: pack_path.to_path_buf(),
            byte_offset: offset,
            compressed_size: comp_len,
            original_size: job.original_size,
            sha256: job.sha256_hex,
            error: None,
        });
        offset += comp_len;
    }

    pack_file.sync_all()?;
    Ok(entries)
}

/// Seeks directly to `[byte_offset, byte_offset + compressed_size]` inside `pack_path`
/// and streams the decompressed Zstandard frame into `dst`.
pub fn extract_slice<W: Write>(
    pack_path: &Path,
    byte_offset: u64,
    compressed_size: u64,
    mut dst: W,
) -> Result<()> {
    let mut file = File::open(pack_path)
        .with_context(|| format!("failed to open pack {}", pack_path.display()))?;
    file.seek(SeekFrom::Start(byte_offset))?;
    let slice_reader = file.take(compressed_size);
    zstd::stream::copy_decode(slice_reader, &mut dst)
        .with_context(|| format!("failed to decompress frame from {}", pack_path.display()))?;
    Ok(())
}

/// Returns the decompressed bytes for a single frame inside `pack_path`.
pub fn extract_bytes(
    pack_path: &Path,
    byte_offset: u64,
    compressed_size: u64,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    extract_slice(pack_path, byte_offset, compressed_size, &mut out)?;
    Ok(out)
}

/// Rewrites `pack_path` keeping only active frames (eliminating dead bytes from revived files)
/// without decompressing or re-compressing any frame.
pub fn compact_pack(
    pack_path: &Path,
    mut active: Vec<ActiveChunk>,
) -> Result<(Vec<ActiveChunk>, u64)> {
    if !pack_path.exists() {
        return Ok((active, 0));
    }
    let old_size = fs::metadata(pack_path)?.len();
    if active.is_empty() {
        let _ = fs::remove_file(pack_path);
        return Ok((active, old_size));
    }

    let total_active: u64 = active.iter().map(|c| c.compressed_size).sum();
    if total_active == old_size {
        for c in &mut active {
            c.new_byte_offset = c.byte_offset;
        }
        return Ok((active, 0));
    }

    let mut src = File::open(pack_path)?;
    let tmp_path = pack_path.with_extension("pack.zst.compact.tmp");
    let mut dst = File::create(&tmp_path)?;

    let mut new_offset = 0u64;
    for chunk in &mut active {
        src.seek(SeekFrom::Start(chunk.byte_offset))?;
        let mut reader = (&mut src).take(chunk.compressed_size);
        let written = std::io::copy(&mut reader, &mut dst)?;
        chunk.new_byte_offset = new_offset;
        new_offset += written;
    }

    dst.sync_all()?;
    drop(dst);
    drop(src);

    fs::rename(&tmp_path, pack_path)?;
    Ok((active, old_size.saturating_sub(new_offset)))
}
