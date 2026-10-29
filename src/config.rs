use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedFolder {
    pub path: String,
    pub slug: String,
    pub older_than: String,
    pub added_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_slayed: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip)]
    pub home_dir: PathBuf,
    pub default_max_age: String,
    pub ignore_patterns: Vec<String>,
    #[serde(default = "default_purge_patterns")]
    pub purge_patterns: Vec<String>,
    pub watched: Vec<WatchedFolder>,
}

pub fn default_ignore_patterns() -> Vec<String> {
    vec![
        ".git",
        ".svn",
        ".hg",
        ".DS_Store",
        ".coldslayer",
        ".agents",
        ".agent",
        ".claude",
        ".gemini",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

pub fn default_purge_patterns() -> Vec<String> {
    vec![
        "node_modules",
        "__pycache__",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
        ".venv",
        "venv",
        "dist",
        "build",
        ".next",
        ".nuxt",
        "target",
        ".turbo",
        ".cache",
        "coverage",
        ".gradle",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

pub fn default_home_dir() -> Result<PathBuf> {
    if let Ok(env_home) = env::var("COLDSLAYER_HOME") {
        if !env_home.trim().is_empty() {
            return Ok(fs::canonicalize(&env_home).unwrap_or_else(|_| PathBuf::from(env_home)));
        }
    }
    let home = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .context("failed to resolve user HOME directory")?;
    Ok(PathBuf::from(home).join(".coldslayer"))
}

impl Config {
    pub fn load() -> Result<Self> {
        let home_dir = default_home_dir()?;
        for sub in ["packs", "catalogs", "tantivy_index"] {
            fs::create_dir_all(home_dir.join(sub))
                .with_context(|| format!("failed to create directory {:?}", home_dir.join(sub)))?;
        }

        let cfg_path = home_dir.join("config.json");
        if !cfg_path.exists() {
            let cfg = Self {
                home_dir,
                default_max_age: "30d".to_string(),
                ignore_patterns: default_ignore_patterns(),
                purge_patterns: default_purge_patterns(),
                watched: Vec::new(),
            };
            cfg.save()?;
            return Ok(cfg);
        }

        let raw = fs::read_to_string(&cfg_path)
            .with_context(|| format!("failed to read config {:?}", cfg_path))?;
        let mut cfg: Config = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse config {:?}", cfg_path))?;
        cfg.home_dir = home_dir;
        if cfg.default_max_age.is_empty() {
            cfg.default_max_age = "30d".to_string();
        }
        if cfg.ignore_patterns.is_empty() {
            cfg.ignore_patterns = default_ignore_patterns();
        }
        if cfg.purge_patterns.is_empty() {
            cfg.purge_patterns = default_purge_patterns();
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let cfg_path = self.home_dir.join("config.json");
        let data = serde_json::to_string_pretty(self)?;
        fs::write(cfg_path, data)?;
        Ok(())
    }

    pub fn entries_store_path(&self) -> PathBuf {
        self.home_dir.join("entries.json")
    }

    pub fn tantivy_dir(&self) -> PathBuf {
        self.home_dir.join("tantivy_index")
    }

    pub fn master_catalog_path(&self) -> PathBuf {
        self.home_dir.join("CATALOG.md")
    }

    pub fn folder_catalog_path(&self, slug: &str) -> PathBuf {
        self.home_dir.join("catalogs").join(format!("{slug}.md"))
    }

    pub fn pack_file_path(&self, slug: &str) -> PathBuf {
        self.home_dir.join("packs").join(format!("{slug}.pack.zst"))
    }

    pub fn add_watched(&mut self, dir: &Path, older_than: &str) -> Result<WatchedFolder> {
        let abs = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        if !abs.exists() {
            return Err(anyhow!("path does not exist: {}", abs.display()));
        }
        if !abs.is_dir() {
            return Err(anyhow!("path is not a directory: {}", abs.display()));
        }
        let dur_str = if older_than.trim().is_empty() {
            self.default_max_age.clone()
        } else {
            older_than.trim().to_string()
        };
        parse_duration(&dur_str)?;

        let abs_str = abs.to_string_lossy().to_string();
        let slug = folder_slug(&abs_str);

        if let Some(existing) = self.watched.iter_mut().find(|w| w.path == abs_str) {
            existing.older_than = dur_str;
            let out = existing.clone();
            self.save()?;
            return Ok(out);
        }

        let wf = WatchedFolder {
            path: abs_str,
            slug,
            older_than: dur_str,
            added_at: Utc::now(),
            last_slayed: None,
        };
        self.watched.push(wf.clone());
        self.save()?;
        Ok(wf)
    }

    pub fn remove_watched(&mut self, dir: &Path) -> Result<bool> {
        let abs = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let abs_str = abs.to_string_lossy().to_string();
        let before = self.watched.len();
        self.watched.retain(|w| w.path != abs_str);
        let removed = self.watched.len() < before;
        self.save()?;
        Ok(removed)
    }
}

static NON_ALNUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9]+").unwrap());

pub fn folder_slug(abs_path: &str) -> String {
    let lower = NON_ALNUM
        .replace_all(abs_path, "-")
        .to_lowercase()
        .trim_matches('-')
        .to_string();
    let trimmed = if lower.len() > 48 {
        lower[lower.len() - 48..].trim_start_matches('-').to_string()
    } else {
        lower
    };

    let mut hasher = Sha1::new();
    hasher.update(abs_path.as_bytes());
    let hash_hex = hex::encode(hasher.finalize());
    let short_hash = &hash_hex[..8];

    if trimmed.is_empty() {
        format!("root-{short_hash}")
    } else {
        format!("{trimmed}-{short_hash}")
    }
}

pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return Ok(Duration::from_secs(30 * 24 * 3600));
    }
    if let Some(num) = s.strip_suffix('d') {
        let days: f64 = num.parse().map_err(|_| anyhow!("invalid duration {s:?}"))?;
        return Ok(Duration::from_secs_f64((days * 86400.0).max(0.0)));
    }
    if let Some(num) = s.strip_suffix('w') {
        let weeks: f64 = num.parse().map_err(|_| anyhow!("invalid duration {s:?}"))?;
        return Ok(Duration::from_secs_f64((weeks * 7.0 * 86400.0).max(0.0)));
    }
    if let Some(num) = s.strip_suffix('h') {
        let hours: f64 = num.parse().map_err(|_| anyhow!("invalid duration {s:?}"))?;
        return Ok(Duration::from_secs_f64((hours * 3600.0).max(0.0)));
    }
    if let Some(num) = s.strip_suffix('m') {
        let mins: f64 = num.parse().map_err(|_| anyhow!("invalid duration {s:?}"))?;
        return Ok(Duration::from_secs_f64((mins * 60.0).max(0.0)));
    }
    if let Some(num) = s.strip_suffix('s') {
        let secs: f64 = num.parse().map_err(|_| anyhow!("invalid duration {s:?}"))?;
        return Ok(Duration::from_secs_f64(secs.max(0.0)));
    }
    Err(anyhow!(
        "invalid duration {s:?} (use e.g. 30d, 2w, 72h, 45m, 10s, or 0d)"
    ))
}

pub fn human_bytes(bytes: u64) -> String {
    const UNIT: f64 = 1024.0;
    if (bytes as f64) < UNIT {
        return format!("{bytes} B");
    }
    let units = ["KB", "MB", "GB", "TB", "PB"];
    let mut val = bytes as f64 / UNIT;
    let mut idx = 0;
    while val >= UNIT && idx + 1 < units.len() {
        val /= UNIT;
        idx += 1;
    }
    format!("{val:.1} {}", units[idx])
}
