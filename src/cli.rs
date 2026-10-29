use crate::config::{folder_slug, human_bytes, parse_duration, Config};
use crate::extractor::{self, Metadata};
use crate::index::{ArchivedFile, SearchOptions, Store};
use crate::packer;
use crate::scanner::{self, ScanOptions};
use crate::skill;
use anyhow::{anyhow, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use filetime::{set_file_times, FileTime};
use rayon::prelude::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "csl",
    version = VERSION,
    about = "Coldslayer (csl) — AI-Native Cold File Archiver with Seekable Zstandard Packs & Tantivy BM25/Fuzzy Search"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize ~/.coldslayer/ and install SKILL.md for Antigravity & Claude Code
    Init {
        /// Also install SKILL.md into the current workspace (.agents/skills/coldslayer/SKILL.md)
        #[arg(long)]
        local: bool,
    },
    /// Register folder(s) to monitor for cold files
    #[command(visible_alias = "add")]
    Watch {
        /// Directories to watch
        dirs: Vec<PathBuf>,
        /// Archive files not accessed within this duration (e.g. 30d, 2w, 720h)
        #[arg(long, default_value = "30d")]
        older_than: String,
    },
    /// Remove folder(s) from the watch list
    #[command(visible_aliases = ["remove", "rm"])]
    Unwatch {
        /// Directories to remove from watch list
        dirs: Vec<PathBuf>,
    },
    /// Scan, purge cold build caches (node_modules, __pycache__, dist, target), compress cold files into 1 .zst pack/folder & index
    #[command(visible_aliases = ["sweep", "archive"])]
    Slay {
        /// Target directories (if omitted, sweeps all watched directories)
        dirs: Vec<PathBuf>,
        /// Archive files not accessed in this duration (default: 30d or folder config)
        #[arg(long, default_value = "")]
        older_than: String,
        /// Preview cold files and disposable cache dirs without compressing or deleting
        #[arg(long)]
        dry_run: bool,
        /// Check only last access time (atime) instead of max(atime, mtime)
        #[arg(long)]
        strict_atime: bool,
        /// Keep uncompressed original files on disk after archiving
        #[arg(long)]
        keep_originals: bool,
        /// Skip auto-deleting cold disposable build/cache folders (node_modules, __pycache__, dist, build, target, .venv)
        #[arg(long)]
        no_purge: bool,
    },
    /// Instant Tantivy BM25 + Phrase Proximity + Fuzzy Typo search across all archived files
    #[command(visible_aliases = ["s", "find", "query"])]
    Search {
        /// Search query words or phrases
        query: Vec<String>,
        /// Filter results by original directory
        #[arg(long)]
        dir: Option<String>,
        /// Filter results by file extension (e.g. rs, go, md, pdf)
        #[arg(long)]
        ext: Option<String>,
        /// Maximum number of results to return
        #[arg(long, default_value_t = 25)]
        limit: usize,
        /// Output search results as JSON for programmatic agent parsing
        #[arg(long)]
        json: bool,
    },
    /// Stream decompressed file content directly to stdout from its byte offset without extracting to disk
    #[command(visible_aliases = ["cat", "show"])]
    Peek {
        /// File ID, filename, or original path
        target: Vec<String>,
    },
    /// Restore an archived file (or --dir <folder>) back to its original path
    #[command(visible_aliases = ["restore", "unarchive"])]
    Revive {
        /// File ID, filename, or original path
        target: Vec<String>,
        /// Restore all archived files belonging to this directory
        #[arg(long)]
        dir: Option<String>,
        /// Keep the file in cold storage after restoring to disk
        #[arg(long)]
        keep: bool,
        /// Custom output file path (when reviving a single file)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Merge/compact all folder .pack.zst files to reclaim dead space
    #[command(visible_alias = "merge")]
    Compact,
    /// Show space saved, watched directories, packs, and catalog locations
    #[command(visible_aliases = ["stats", "info", "list", "ls"])]
    Status {
        /// Output status as JSON
        #[arg(long)]
        json: bool,
    },
    /// Print version information
    Version,
}

pub fn run_from_args<I, T>(args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into< std::ffi::OsString> + Clone,
{
    let raw_vec: Vec<String> = args
        .into_iter()
        .map(|x| x.into().to_string_lossy().to_string())
        .collect();

    if raw_vec.len() <= 1 || matches!(raw_vec.get(1).map(|s| s.as_str()), Some("help" | "-h" | "--help")) {
        let mut norm = vec![raw_vec.first().cloned().unwrap_or_else(|| "csl".to_string())];
        norm.push("--help".to_string());
        let _ = Cli::try_parse_from(norm);
        print_usage();
        return Ok(());
    }

    if matches!(raw_vec.get(1).map(|s| s.as_str()), Some("version" | "-v" | "--version")) {
        println!(
            "csl (coldslayer) v{} ({}/{})",
            VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return Ok(());
    }

    let cli = Cli::try_parse_from(&raw_vec)?;
    match cli.command {
        Some(Commands::Init { local }) => run_init(local),
        Some(Commands::Watch { dirs, older_than }) => run_watch(&dirs, &older_than),
        Some(Commands::Unwatch { dirs }) => run_unwatch(&dirs),
        Some(Commands::Slay {
            dirs,
            older_than,
            dry_run,
            strict_atime,
            keep_originals,
            no_purge,
        }) => run_slay(
            &dirs,
            &older_than,
            dry_run,
            strict_atime,
            keep_originals,
            !no_purge,
        ),
        Some(Commands::Search {
            query,
            dir,
            ext,
            limit,
            json,
        }) => run_search(&query.join(" "), dir, ext, limit, json),
        Some(Commands::Peek { target }) => run_peek(&target.join(" ")),
        Some(Commands::Revive {
            target,
            dir,
            keep,
            out,
        }) => run_revive(&target.join(" "), dir, keep, out),
        Some(Commands::Compact) => run_compact(),
        Some(Commands::Status { json }) => run_status(json),
        Some(Commands::Version) => {
            println!(
                "csl (coldslayer) v{} ({}/{})",
                VERSION,
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            Ok(())
        }
        None => {
            print_usage();
            Ok(())
        }
    }
}

fn run_init(local_skill: bool) -> Result<()> {
    let cfg = Config::load()?;
    let store = Store::open(cfg.clone())?;
    store.sync_catalogs()?;

    let installed = skill::install(local_skill)?;

    println!("Coldslayer (csl) initialized at {}", cfg.home_dir.display());
    println!("  Tantivy Index:  {}", cfg.tantivy_dir().display());
    println!("  Master Catalog: {}", cfg.master_catalog_path().display());
    println!(
        "  Packs Vault:    {}",
        cfg.home_dir.join("packs").display()
    );
    println!("\nInstalled AI Agent Skill (SKILL.md):");
    for p in installed {
        println!("  + {}", p.display());
    }
    println!("\nNext steps:");
    println!("  1. Watch a folder:   csl watch ~/Downloads --older-than 30d");
    println!("  2. Preview cold:     csl slay ~/Downloads --older-than 30d --dry-run");
    println!("  3. Archive cold:     csl slay ~/Downloads --older-than 30d");
    Ok(())
}

fn run_watch(dirs: &[PathBuf], older_than: &str) -> Result<()> {
    if dirs.is_empty() {
        return Err(anyhow!("usage: csl watch <dir> [dir2...] [--older-than 30d]"));
    }
    let mut cfg = Config::load()?;
    for d in dirs {
        let wf = cfg.add_watched(d, older_than)?;
        println!(
            "Watching {} (threshold: {}, pack: {})",
            wf.path,
            wf.older_than,
            cfg.pack_file_path(&wf.slug).display()
        );
    }
    Ok(())
}

fn run_unwatch(dirs: &[PathBuf]) -> Result<()> {
    if dirs.is_empty() {
        return Err(anyhow!("usage: csl unwatch <dir>"));
    }
    let mut cfg = Config::load()?;
    for d in dirs {
        if cfg.remove_watched(d)? {
            println!("Removed {} from watched directories.", d.display());
        } else {
            println!("Directory {} was not in watched list.", d.display());
        }
    }
    Ok(())
}

struct TargetSweep {
    path: PathBuf,
    slug: String,
    older_than: Duration,
}

fn run_slay(
    dirs: &[PathBuf],
    older_than_str: &str,
    dry_run: bool,
    strict_atime: bool,
    keep_originals: bool,
    enable_purge: bool,
) -> Result<()> {
    let mut cfg = Config::load()?;
    let mut store = Store::open(cfg.clone())?;

    let mut targets = Vec::new();
    if !dirs.is_empty() {
        let dur_str = if older_than_str.is_empty() {
            &cfg.default_max_age
        } else {
            older_than_str
        };
        let dur = parse_duration(dur_str)?;
        for d in dirs {
            let abs = fs::canonicalize(d).unwrap_or_else(|_| d.clone());
            let abs_str = abs.to_string_lossy().to_string();
            targets.push(TargetSweep {
                path: abs,
                slug: folder_slug(&abs_str),
                older_than: dur,
            });
        }
    } else {
        if cfg.watched.is_empty() {
            return Err(anyhow!(
                "no folders specified and no watched folders configured.\nRun 'csl slay <folder> --older-than 30d' or 'csl watch <folder>'"
            ));
        }
        for w in &cfg.watched {
            let dur_str = if older_than_str.is_empty() {
                &w.older_than
            } else {
                older_than_str
            };
            let dur = parse_duration(dur_str)?;
            targets.push(TargetSweep {
                path: PathBuf::from(&w.path),
                slug: w.slug.clone(),
                older_than: dur,
            });
        }
    }

    let start_time = Instant::now();
    let mut grand_total_files = 0usize;
    let mut grand_orig_bytes = 0u64;
    let mut grand_comp_bytes = 0u64;
    let mut grand_purged_dirs = 0usize;
    let mut grand_purged_bytes = 0u64;

    for t in &targets {
        println!(
            "Scanning {} (threshold: {:?})...",
            t.path.display(),
            t.older_than
        );
        let scan_res = scanner::scan(&ScanOptions {
            root_dir: t.path.clone(),
            older_than: t.older_than,
            ignore_patterns: cfg.ignore_patterns.clone(),
            purge_patterns: cfg.purge_patterns.clone(),
            strict_atime,
            enable_purge,
        })?;

        // 1. Report & purge cold disposable build/cache directories (node_modules, __pycache__, dist, build, target, .venv)
        if !scan_res.purge_dirs.is_empty() {
            let purge_bytes: u64 = scan_res.purge_dirs.iter().map(|d| d.size_bytes).sum();
            println!(
                "  Found {} cold disposable build/cache folder(s) ({}):",
                scan_res.purge_dirs.len(),
                human_bytes(purge_bytes)
            );
            for pd in &scan_res.purge_dirs {
                println!(
                    "    [purge] {}/ ({})",
                    pd.rel_path,
                    human_bytes(pd.size_bytes)
                );
                if !dry_run {
                    if fs::remove_dir_all(&pd.abs_path).is_ok() {
                        grand_purged_dirs += 1;
                        grand_purged_bytes += pd.size_bytes;
                    }
                }
            }
        }

        let candidates = scan_res.files;
        if candidates.is_empty() {
            println!("  No cold source/doc files found in {}.", t.path.display());
            continue;
        }

        let folder_orig_bytes: u64 = candidates.iter().map(|c| c.size).sum();
        println!(
            "  Found {} cold file(s) ({}), sorted by last accessed:",
            candidates.len(),
            human_bytes(folder_orig_bytes)
        );
        for (i, c) in candidates.iter().take(12).enumerate() {
            println!(
                "    [{}] {} ({} | last accessed: {})",
                i + 1,
                c.rel_path,
                human_bytes(c.size),
                c.last_accessed.format("%Y-%m-%d")
            );
        }
        if candidates.len() > 12 {
            println!("    ... and {} more file(s)", candidates.len() - 12);
        }

        if dry_run {
            continue;
        }

        // Step 1: Extract full vocabulary + keywords + symbols + summary in parallel via Rayon
        let metas: Vec<Metadata> = candidates
            .par_iter()
            .map(|c| extractor::extract(&c.abs_path))
            .collect();

        // Step 2: Compress in parallel and append to the folder's single merged .pack.zst
        let file_paths: Vec<PathBuf> = candidates.iter().map(|c| c.abs_path.clone()).collect();
        let pack_path = cfg.pack_file_path(&t.slug);
        let packed_entries = packer::append_batch(&pack_path, &file_paths)?;

        // Step 3: Build index entries and remove originals
        let archived_now = Utc::now();
        let mut index_batch = Vec::with_capacity(packed_entries.len());
        let mut folder_comp_bytes = 0u64;

        for (i, pe) in packed_entries.into_iter().enumerate() {
            if let Some(err) = pe.error {
                eprintln!("  ! Skipping {}: {}", candidates[i].rel_path, err);
                continue;
            }
            let c = &candidates[i];
            let m = &metas[i];
            index_batch.push(ArchivedFile {
                id: 0,
                abs_path: c.abs_path.to_string_lossy().to_string(),
                rel_path: c.rel_path.clone(),
                root_dir: t.path.to_string_lossy().to_string(),
                folder_slug: t.slug.clone(),
                file_name: c.name.clone(),
                ext: c.ext.clone(),
                category: m.category.clone(),
                summary: m.summary.clone(),
                symbols: m.symbols.join(", "),
                keywords: m.keywords.join(", "),
                body_vocab: m.body_vocab.clone(),
                original_size: pe.original_size,
                compressed_size: pe.compressed_size,
                pack_path: pe.pack_path.to_string_lossy().to_string(),
                byte_offset: pe.byte_offset,
                sha256: pe.sha256,
                file_mode: c.mode,
                last_accessed: c.last_accessed,
                last_modified: c.last_modified,
                archived_at: archived_now,
                score: 0.0,
                matched_snippet: String::new(),
            });
            folder_comp_bytes += pe.compressed_size;
        }

        store.upsert_batch(index_batch.clone())?;

        if !keep_originals {
            for af in &index_batch {
                let _ = fs::remove_file(&af.abs_path);
            }
        }

        let _ = store.compact_folder_pack(&t.slug);

        let root_str = t.path.to_string_lossy().to_string();
        for w in &mut cfg.watched {
            if w.path == root_str {
                w.last_slayed = Some(archived_now);
            }
        }

        grand_total_files += index_batch.len();
        grand_orig_bytes += folder_orig_bytes;
        grand_comp_bytes += folder_comp_bytes;

        println!(
            "  Archived {} file(s) -> {} ({} -> {})",
            index_batch.len(),
            pack_path.display(),
            human_bytes(folder_orig_bytes),
            human_bytes(folder_comp_bytes)
        );
    }

    if dry_run {
        println!("\nDry run complete. No files were modified.");
        return Ok(());
    }

    let _ = cfg.save();
    store.sync_catalogs()?;

    if grand_total_files > 0 || grand_purged_dirs > 0 {
        let archive_saved = grand_orig_bytes.saturating_sub(grand_comp_bytes);
        let total_saved = archive_saved + grand_purged_bytes;
        let ratio = if grand_orig_bytes > grand_comp_bytes && grand_orig_bytes > 0 {
            (1.0 - (grand_comp_bytes as f64 / grand_orig_bytes as f64)) * 100.0
        } else {
            0.0
        };
        println!(
            "\nSlay complete in {:?}: {} file(s) archived ({:.1}% compression), {} cache dir(s) purged ({}), total disk space reclaimed: {}.",
            start_time.elapsed(),
            grand_total_files,
            ratio,
            grand_purged_dirs,
            human_bytes(grand_purged_bytes),
            human_bytes(total_saved)
        );
        println!(
            "Centralized catalog updated: {}",
            cfg.master_catalog_path().display()
        );
    }
    Ok(())
}

fn run_search(
    query: &str,
    dir: Option<String>,
    ext: Option<String>,
    limit: usize,
    json_out: bool,
) -> Result<()> {
    let cfg = Config::load()?;
    let store = Store::open(cfg)?;

    let start = Instant::now();
    let results = store.search(&SearchOptions {
        query: query.to_string(),
        dir,
        ext,
        limit,
    })?;
    let elapsed = start.elapsed();

    if json_out {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("No archived files matched {query:?} (searched in {elapsed:?}).");
        return Ok(());
    }

    println!("Found {} match(es) in {:?}:\n", results.len(), elapsed);
    for r in results {
        println!(
            "[{}] (score: {:.2}) {}  ({} -> {} | last accessed: {} | archived: {})",
            r.id,
            r.score,
            r.abs_path,
            human_bytes(r.original_size),
            human_bytes(r.compressed_size),
            r.last_accessed.format("%Y-%m-%d"),
            r.archived_at.format("%Y-%m-%d")
        );
        if !r.keywords.is_empty() {
            println!("    Keywords: {}", r.keywords);
        }
        if !r.symbols.is_empty() {
            println!("    Symbols:  {}", r.symbols);
        }
        if !r.matched_snippet.is_empty() {
            println!("    Snippet:  {}", r.matched_snippet);
        } else {
            println!("    Summary:  {}", r.summary);
        }
        println!("    Actions:  csl peek {}  |  csl revive {}\n", r.id, r.id);
    }
    Ok(())
}

fn run_peek(target: &str) -> Result<()> {
    if target.trim().is_empty() {
        return Err(anyhow!("usage: csl peek <id-or-path>"));
    }
    let cfg = Config::load()?;
    let store = Store::open(cfg)?;
    let entry = store.resolve_target(target)?;
    let stdout = io::stdout();
    let handle = stdout.lock();
    packer::extract_slice(
        Path::new(&entry.pack_path),
        entry.byte_offset,
        entry.compressed_size,
        handle,
    )
}

fn run_revive(
    target: &str,
    dir_target: Option<String>,
    keep: bool,
    out_path: Option<PathBuf>,
) -> Result<()> {
    let cfg = Config::load()?;
    let mut store = Store::open(cfg)?;

    let entries: Vec<ArchivedFile> = if let Some(ref d) = dir_target {
        let items = store.list_recent(Some(d), None, 100_000);
        if items.is_empty() {
            return Err(anyhow!("no archived files found under directory {d}"));
        }
        items
    } else {
        if target.trim().is_empty() {
            return Err(anyhow!(
                "usage: csl revive <id-or-path> [--keep] [--out <path>] OR csl revive --dir <folder>"
            ));
        }
        vec![store.resolve_target(target)?]
    };

    let mut affected_slugs = HashSet::new();
    for entry in &entries {
        let dest = if let Some(ref custom) = out_path {
            if entries.len() == 1 {
                custom.clone()
            } else {
                PathBuf::from(&entry.abs_path)
            }
        } else {
            PathBuf::from(&entry.abs_path)
        };

        let raw = packer::extract_bytes(
            Path::new(&entry.pack_path),
            entry.byte_offset,
            entry.compressed_size,
        )?;

        if !entry.sha256.is_empty() {
            let mut hasher = Sha256::new();
            hasher.update(&raw);
            let actual = hex::encode(hasher.finalize());
            if actual != entry.sha256 {
                return Err(anyhow!("checksum mismatch while reviving {}", entry.abs_path));
            }
        }

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, &raw)?;
        restore_permissions(&dest, entry.file_mode);

        let now_ft = FileTime::from_system_time(SystemTime::now());
        let mod_ft = FileTime::from_unix_time(entry.last_modified.timestamp(), 0);
        let _ = set_file_times(&dest, now_ft, mod_ft);

        if !keep {
            store.delete_by_id(entry.id)?;
            affected_slugs.insert(entry.folder_slug.clone());
        }
        println!(
            "Revived: {} ({})",
            dest.display(),
            human_bytes(entry.original_size)
        );
    }

    for slug in affected_slugs {
        let _ = store.compact_folder_pack(&slug);
    }
    store.sync_catalogs()?;
    Ok(())
}

fn run_compact() -> Result<()> {
    let cfg = Config::load()?;
    let mut store = Store::open(cfg)?;
    let stats = store.get_folder_stats();
    let mut total_reclaimed = 0u64;

    for st in stats {
        let reclaimed = store.compact_folder_pack(&st.folder_slug)?;
        total_reclaimed += reclaimed;
        println!(
            "Pack {}: {} file(s), {} compressed (reclaimed {})",
            st.pack_path,
            st.file_count,
            human_bytes(st.compressed_size),
            human_bytes(reclaimed)
        );
    }
    store.sync_catalogs()?;
    println!(
        "Compaction finished. Total dead space reclaimed: {}",
        human_bytes(total_reclaimed)
    );
    Ok(())
}

fn run_status(json_out: bool) -> Result<()> {
    let cfg = Config::load()?;
    let store = Store::open(cfg.clone())?;
    let stats = store.get_folder_stats();

    let total_files: usize = stats.iter().map(|s| s.file_count).sum();
    let total_orig: u64 = stats.iter().map(|s| s.original_size).sum();
    let total_comp: u64 = stats.iter().map(|s| s.compressed_size).sum();
    let saved = total_orig.saturating_sub(total_comp);

    if json_out {
        let payload = json!({
            "home_dir": cfg.home_dir,
            "tantivy_index": cfg.tantivy_dir(),
            "master_catalog": cfg.master_catalog_path(),
            "total_files": total_files,
            "original_size": total_orig,
            "compressed_size": total_comp,
            "saved_bytes": saved,
            "watched": cfg.watched,
            "folders": stats,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("Coldslayer Status (v{})", VERSION);
    println!("  Home Directory:  {}", cfg.home_dir.display());
    println!("  Tantivy Index:   {}", cfg.tantivy_dir().display());
    println!("  Master Catalog:  {}", cfg.master_catalog_path().display());
    println!("  Total Archived:  {} file(s)", total_files);
    println!("  Original Size:   {}", human_bytes(total_orig));
    println!("  Compressed Size: {}", human_bytes(total_comp));
    println!("  Space Saved:     {}\n", human_bytes(saved));

    println!("Watched Directories:");
    if cfg.watched.is_empty() {
        println!("  (none configured — run 'csl watch <folder>')");
    } else {
        for w in &cfg.watched {
            let last = w
                .last_slayed
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "never".to_string());
            println!(
                "  - {} (older-than: {} | last slayed: {})",
                w.path, w.older_than, last
            );
        }
    }

    if !stats.is_empty() {
        println!("\nArchived Folder Packs:");
        for st in stats {
            println!(
                "  - {}\n    Files: {} | {} -> {} | Pack: {}\n    Catalog: {}",
                st.root_dir,
                st.file_count,
                human_bytes(st.original_size),
                human_bytes(st.compressed_size),
                st.pack_path,
                cfg.folder_catalog_path(&st.folder_slug).display()
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn restore_permissions(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if mode != 0 {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
}

#[cfg(not(unix))]
fn restore_permissions(_path: &Path, _mode: u32) {}

fn print_usage() {
    println!(
        r#"csl (Coldslayer) — AI-Native Cold File Archiver & Ultra-Fast Local Tantivy Search

Usage:
  csl <command> [arguments] [flags]

Core Commands:
  init [--local]                         Initialize ~/.coldslayer/ and install SKILL.md for Antigravity & Claude
  watch <dir...> [--older-than 30d]      Register folder(s) to monitor for cold files
  unwatch <dir...>                       Remove folder(s) from the watch list
  slay [dir...] [--older-than 30d]       Purge cold build caches (node_modules, __pycache__, dist, target),
       [--dry-run] [--no-purge]          compress cold files into 1 .zst pack/folder & index in Tantivy
  search <query> [--dir <d>] [--ext <e>] Instant Tantivy BM25 + Phrase + Fuzzy Typo search across all files
       [--limit 25] [--json]             (Short alias: csl s <query>)
  peek <id-or-path>                      Stream decompressed file content to stdout without extracting to disk
  revive <id-or-path> [--keep]           Restore an archived file (or --dir <folder>) back to its original path
  compact                                Merge/compact all folder packs to reclaim dead space
  status [--json]                        Show space saved, watched directories, packs, and catalog locations
  version                                Print version information"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn test_end_to_end_rust_coldslayer_flow() {
        let base_tmp = std::env::temp_dir().join(format!(
            "coldslayer_rust_test_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home_dir = base_tmp.join("home");
        let work_dir = base_tmp.join("workspace");
        fs::create_dir_all(&home_dir).unwrap();
        fs::create_dir_all(&work_dir).unwrap();

        std::env::set_var("COLDSLAYER_HOME", &home_dir);

        // 1. Create cold files (45 days old), a cold node_modules folder, and a hot file
        let cold_file_1 = work_dir.join("payment_webhook_handler.rs");
        let mut filler_lines = String::new();
        for i in 0..40 {
            filler_lines.push_str(&format!("// standard helper comment line {}\n", i));
        }
        let cold_content_1 = format!(
            "pub fn handle_stripe_webhook(payload: &[u8]) {{\n{}\n    let secret_token = \"xyz_deep_buried_identifier_99\";\n}}\n",
            filler_lines
        );
        fs::write(&cold_file_1, &cold_content_1).unwrap();

        let cold_file_2 = work_dir.join("payment_retry_worker.rs");
        let cold_content_2 =
            "pub fn retry_payment() { /* handles payment retries without webhook */ }\n";
        fs::write(&cold_file_2, cold_content_2).unwrap();

        let cold_node_modules = work_dir.join("node_modules").join("left-pad");
        fs::create_dir_all(&cold_node_modules).unwrap();
        let nm_file = cold_node_modules.join("index.js");
        fs::write(&nm_file, "module.exports = function() {};").unwrap();

        let hot_file = work_dir.join("active_todo.md");
        fs::write(&hot_file, "# Active work today").unwrap();

        let old_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - (45 * 86400);
        let old_ft = FileTime::from_unix_time(old_ts, 0);

        set_file_times(&cold_file_1, old_ft, old_ft).unwrap();
        set_file_times(&cold_file_2, old_ft, old_ft).unwrap();
        set_file_times(&nm_file, old_ft, old_ft).unwrap();
        set_file_times(&cold_node_modules, old_ft, old_ft).unwrap();
        set_file_times(work_dir.join("node_modules"), old_ft, old_ft).unwrap();

        // 2. Run `csl slay`
        run_from_args([
            "csl",
            "slay",
            work_dir.to_str().unwrap(),
            "--older-than",
            "30d",
        ])
        .unwrap();

        // Verify cold files and cold node_modules were removed, while hot_file remains
        assert!(!cold_file_1.exists(), "cold_file_1 should be archived");
        assert!(!cold_file_2.exists(), "cold_file_2 should be archived");
        assert!(
            !work_dir.join("node_modules").exists(),
            "cold node_modules should be auto-purged"
        );
        assert!(hot_file.exists(), "hot_file should remain untouched");

        let cfg = Config::load().unwrap();
        let store = Store::open(cfg).unwrap();

        // 3. Verify Issue #3: Exact phrase / proximity ("stripe webhook") ranks cold_file_1 first
        let res_phrase = store
            .search(&SearchOptions {
                query: "payment webhook".to_string(),
                dir: None,
                ext: None,
                limit: 10,
            })
            .unwrap();
        assert!(!res_phrase.is_empty());
        assert_eq!(res_phrase[0].file_name, "payment_webhook_handler.rs");
        assert!(res_phrase[0].score > res_phrase.get(1).map(|r| r.score).unwrap_or(0.0));

        // 4. Verify Issue #4: Full vocabulary indexing finds `xyz_deep_buried_identifier_99`
        let res_vocab = store
            .search(&SearchOptions {
                query: "xyz_deep_buried_identifier_99".to_string(),
                dir: None,
                ext: None,
                limit: 10,
            })
            .unwrap();
        assert_eq!(res_vocab.len(), 1);
        assert_eq!(res_vocab[0].file_name, "payment_webhook_handler.rs");

        // 5. Verify Issue #5: Fuzzy typo tolerance ("webhok") and substring ("hook") match
        let res_typo = store
            .search(&SearchOptions {
                query: "webhok".to_string(),
                dir: None,
                ext: None,
                limit: 10,
            })
            .unwrap();
        assert!(!res_typo.is_empty(), "fuzzy typo 'webhok' should match 'webhook'");
        assert_eq!(res_typo[0].file_name, "payment_webhook_handler.rs");

        let res_sub = store
            .search(&SearchOptions {
                query: "hook".to_string(),
                dir: None,
                ext: None,
                limit: 10,
            })
            .unwrap();
        assert!(!res_sub.is_empty(), "mid-word substring 'hook' should match 'webhook'");

        // 6. Verify Issue #6: Highlighted snippet and positive score are populated
        assert!(res_phrase[0].score > 0.0);
        assert!(!res_phrase[0].matched_snippet.is_empty());

        // 7. Revive cold_file_1 and verify exact content restoration
        run_from_args(["csl", "revive", cold_file_1.to_str().unwrap()]).unwrap();
        let restored = fs::read_to_string(&cold_file_1).unwrap();
        assert_eq!(restored, cold_content_1);

        let _ = fs::remove_dir_all(&base_tmp);
    }
}
