use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use filetime::FileTime;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone)]
pub struct CandidateFile {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub name: String,
    pub ext: String,
    pub size: u64,
    pub mode: u32,
    pub last_accessed: DateTime<Utc>,
    pub last_modified: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PurgeCandidate {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub files: Vec<CandidateFile>,
    pub purge_dirs: Vec<PurgeCandidate>,
}

pub struct ScanOptions {
    pub root_dir: PathBuf,
    pub older_than: Duration,
    pub ignore_patterns: Vec<String>,
    pub purge_patterns: Vec<String>,
    pub strict_atime: bool,
    pub enable_purge: bool,
}

pub fn scan(opts: &ScanOptions) -> Result<ScanResult> {
    let abs_root = fs::canonicalize(&opts.root_dir).unwrap_or_else(|_| opts.root_dir.clone());
    let now = SystemTime::now();
    let cutoff_secs = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(opts.older_than.as_secs()) as i64;
    let check_cutoff = opts.older_than.as_secs() > 0;

    let ignore_set: HashSet<String> = opts
        .ignore_patterns
        .iter()
        .map(|s| s.trim().to_string())
        .collect();
    let purge_set: HashSet<String> = opts
        .purge_patterns
        .iter()
        .map(|s| s.trim().to_string())
        .collect();

    let mut file_paths: Vec<PathBuf> = Vec::new();
    let mut raw_purge_dirs: Vec<PathBuf> = Vec::new();

    let mut it = WalkDir::new(&abs_root).follow_links(false).into_iter();
    while let Some(entry_res) = it.next() {
        let entry: DirEntry = match entry_res {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path == abs_root {
            continue;
        }
        let name = entry.file_name().to_string_lossy();

        if ignore_set.contains(name.as_ref()) {
            if entry.file_type().is_dir() {
                it.skip_current_dir();
            }
            continue;
        }

        if entry.file_type().is_dir() && purge_set.contains(name.as_ref()) {
            if opts.enable_purge {
                raw_purge_dirs.push(path.to_path_buf());
            }
            it.skip_current_dir();
            continue;
        }

        if entry.file_type().is_file() {
            file_paths.push(path.to_path_buf());
        }
    }

    // Parallel stat & filter of regular files across all CPU cores via Rayon
    let mut files: Vec<CandidateFile> = file_paths
        .par_iter()
        .filter_map(|p| {
            let meta = fs::symlink_metadata(p).ok()?;
            if !meta.is_file() {
                return None;
            }
            let atime = FileTime::from_last_access_time(&meta).unix_seconds();
            let mtime = FileTime::from_last_modification_time(&meta).unix_seconds();
            let last_active = if opts.strict_atime {
                atime
            } else {
                atime.max(mtime)
            };

            if check_cutoff && last_active > cutoff_secs {
                return None;
            }

            let rel = p
                .strip_prefix(&abs_root)
                .unwrap_or(p)
                .to_string_lossy()
                .to_string();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            let mode = extract_mode(&meta);

            Some(CandidateFile {
                abs_path: p.clone(),
                rel_path: rel,
                name,
                ext,
                size: meta.len(),
                mode,
                last_accessed: Utc.timestamp_opt(atime, 0).single().unwrap_or_else(Utc::now),
                last_modified: Utc.timestamp_opt(mtime, 0).single().unwrap_or_else(Utc::now),
            })
        })
        .collect();

    // Sort cold files from oldest accessed to newest accessed
    files.sort_by(|a, b| {
        a.last_accessed
            .cmp(&b.last_accessed)
            .then_with(|| a.abs_path.cmp(&b.abs_path))
    });

    // Evaluate disposable build/cache directories in parallel
    let mut purge_dirs: Vec<PurgeCandidate> = raw_purge_dirs
        .par_iter()
        .filter_map(|dir_path| {
            let (total_size, latest_ts) = inspect_dir_size_and_activity(dir_path, opts.strict_atime);
            if check_cutoff && latest_ts > cutoff_secs {
                return None;
            }
            let rel = dir_path
                .strip_prefix(&abs_root)
                .unwrap_or(dir_path)
                .to_string_lossy()
                .to_string();
            Some(PurgeCandidate {
                abs_path: dir_path.clone(),
                rel_path: rel,
                size_bytes: total_size,
            })
        })
        .collect();

    purge_dirs.sort_by(|a, b| a.abs_path.cmp(&b.abs_path));

    Ok(ScanResult { files, purge_dirs })
}

fn inspect_dir_size_and_activity(dir: &Path, strict_atime: bool) -> (u64, i64) {
    let mut total_size = 0u64;
    let mut latest_ts = i64::MIN;

    if let Ok(meta) = fs::symlink_metadata(dir) {
        let mtime = FileTime::from_last_modification_time(&meta).unix_seconds();
        latest_ts = latest_ts.max(mtime);
    }

    for entry in WalkDir::new(dir).follow_links(false).into_iter().flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total_size += meta.len();
                let atime = FileTime::from_last_access_time(&meta).unix_seconds();
                let mtime = FileTime::from_last_modification_time(&meta).unix_seconds();
                let active = if strict_atime { atime } else { atime.max(mtime) };
                latest_ts = latest_ts.max(active);
            }
        }
    }
    if latest_ts == i64::MIN {
        latest_ts = 0;
    }
    (total_size, latest_ts)
}

#[cfg(unix)]
fn extract_mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

#[cfg(not(unix))]
fn extract_mode(_meta: &fs::Metadata) -> u32 {
    0o644
}
