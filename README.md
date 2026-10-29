<p align="center">
  <img src="./assets/coldslayer-logo.svg" alt="Coldslayer Logo" width="120" height="120" />
</p>

<h1 align="center">Coldslayer (<code>csl</code>)</h1>

<p align="center">
  <strong>AI-Native Cold File Archiver written in Rust with Single-Pack Seekable Zstandard Compression & Embedded Tantivy BM25 + Fuzzy Search.</strong>
</p>

---

Traditional disk cleanup utilities compress old files into `.tar.gz` or `.zip` archives—making AI coding agents (**Antigravity**, **Claude Code**, **Cursor**) completely blind to them, and requiring slow full-archive decompression just to inspect or restore a single file.

**Coldslayer (`csl`)** solves this **100% locally in pure Rust** (zero servers, zero API keys required):
* **Fast 3-Letter CLI (`csl`):** Every command is built around the `csl` CLI (with `coldslayer` also installed as an alias).
* **Multi-Core Parallel Scanner (`rayon` + `walkdir`):** Scans target directories across all CPU cores and sorts files by when they were last accessed (`atime` / `mtime`).
* **Auto-Purge Cold Build/Cache Junk:** Automatically removes stale disposable build/cache folders (`node_modules`, `__pycache__`, `.venv`, `venv`, `dist`, `build`, `target`, `.next`, `.nuxt`, `.turbo`, `.cache`) in cold projects while keeping `.git` and active folders untouched.
* **Full-Vocabulary + Keyword & Symbol Extraction:** Extracts filenames, file categories, Markdown headings, code symbols (`fn`, `func`, `class`, `interface`, `type`, `struct`, `def`), top TF-IDF weighted keywords, AND the **full deduplicated vocabulary** of the file.
* **One Merged Seekable `.pack.zst` Per Folder:** Compresses cold files in parallel across all CPU cores into independent Zstandard frames appended to `~/.coldslayer/packs/<folder-slug>.pack.zst`. Subsequent runs automatically merge into the same pack without decompressing existing files, and single files are read or restored in `<1ms` by seeking directly to `[byte_offset, compressed_size]`.
* **Embedded Tantivy Search Engine (Lucene in Rust) + Centralized `CATALOG.md`:**
  * **Phrase & Proximity (`NEAR`) Boosting:** Multi-word queries rank exact phrases and close-proximity terms at the top.
  * **Fuzzy Typo Tolerance & Substring Matching:** Handles 1–2 character Levenshtein typos (`"webhok"` → `"webhook"`) and mid-word substrings (`"hook"` → `"webhook"`).
  * **Score & Snippet Highlighting:** Displays relevance scores and `>>highlighted<<` match snippets.
* **Built-in AI Agent Skill (`SKILL.md`):** Running `csl init` automatically installs the `coldslayer` skill into `~/.gemini/config/skills/coldslayer/SKILL.md` (Antigravity) and `~/.claude/skills/coldslayer/SKILL.md` (Claude Code).

---

## Installation Instructions

Choose any of the 3 methods below to install `csl`:

### Option 1: Homebrew (macOS & Linux)
```bash
brew tap xlogn/tap
brew trust xlogn/tap   # Required on Homebrew 5+ for third-party taps
brew install coldslayer
csl init
```

### Option 2: One-Line Installer (macOS & Linux)
Installs the `csl` binary (plus the `coldslayer` alias) and runs `csl init` automatically:
```bash
curl -fsSL https://raw.githubusercontent.com/xlogn/coldslayer/main/install.sh | sh
```

### Option 3: Clone & Build from Source (Rust / Cargo)
```bash
git clone https://github.com/xlogn/coldslayer.git
cd coldslayer
cargo build --release
cp target/release/csl /opt/homebrew/bin/csl   # or /usr/local/bin/csl
csl init
```

---

## Step-by-Step Usage Guide (`csl`)

### Step 1: Initialize Coldslayer & Install the AI Agent Skill
Run once after installation:
```bash
csl init
```
**What this does:**
1. Creates `~/.coldslayer/` (`config.json`, `entries.json`, `tantivy_index/`, `CATALOG.md`, `packs/`, `catalogs/`).
2. Installs the `coldslayer` AI Agent Skill into:
   * `~/.gemini/config/skills/coldslayer/SKILL.md` (Google Antigravity)
   * `~/.claude/skills/coldslayer/SKILL.md` (Claude Code)
   * *(Pass `csl init --local` if you also want `.agents/skills/coldslayer/SKILL.md` inside your current repo).*

---

### Step 2: Register Folders or Slay Cold Files Directly

#### A. Preview Cold Files & Disposable Cache Folders First (`--dry-run`)
See which files and build caches (`node_modules`, `target`, `__pycache__`, `dist`) haven't been accessed in the last 30 days without modifying anything on disk:
```bash
csl slay ~/Downloads --older-than 30d --dry-run
```

#### B. Compress Cold Files & Purge Stale Build Caches (`slay`)
Purge cold build caches, compress cold files into `~/.coldslayer/packs/`, index full vocabulary + keywords in Tantivy + `CATALOG.md`, and remove the uncompressed originals:
```bash
csl slay ~/Downloads --older-than 30d

# Optional: skip purging node_modules/__pycache__/dist/target if desired
csl slay ~/Downloads --older-than 30d --no-purge
```
*(Supported duration formats: `30d`, `2w`, `72h`, `45m`, or `0d` to archive all files immediately).*

#### C. Watch Multiple Folders for Recurring Sweeps
Register folders once so you (or a cron job) can simply run `csl slay` with no arguments:
```bash
csl watch ~/Downloads ~/Documents/OldProjects --older-than 30d

# Sweep all watched folders at once
csl slay

# Remove a folder from the watch list
csl unwatch ~/Downloads
```

---

### Step 3: Search Cold Storage (`<5ms` Tantivy BM25 + Phrase + Fuzzy Typo Search)
Search across filenames, full file vocabulary, extracted keywords, code symbols, Markdown headings, and summaries using `csl search` or the short alias `csl s`:
```bash
# Full-text BM25 + Phrase Proximity + Fuzzy Typo search
csl s "payment webhook"
csl search "payment webhook"

# Filter by original directory or file extension
csl s "architecture" --dir ~/Downloads --ext md

# Output structured JSON (ideal for AI agents and scripts)
csl s "stripe" --json
```

---

### Step 4: Peek or Restore (`revive`) Files in `<1ms`

Because Coldslayer stores the exact `[byte_offset, compressed_size]` of every file inside its `.pack.zst` archive, it never has to unzip the entire archive:

```bash
# Stream a file's contents directly to stdout WITHOUT extracting it to disk
csl peek 1
csl peek "/Users/you/Downloads/architecture_notes.md"

# Restore a file back to its exact original path (verifies SHA-256 & compacts the pack)
csl revive "/Users/you/Downloads/architecture_notes.md"

# Restore a file to disk while ALSO keeping it in cold storage
csl revive 1 --keep

# Restore ALL archived files belonging to a directory
csl revive --dir ~/Downloads
```

---

### Step 5: View Status & Compact Packs
```bash
# Check total disk space saved, watched directories, and catalog paths
csl status

# Manually compact all .pack.zst files to reclaim any dead space
csl compact
```

---

## How the Centralized Storage Works (`~/.coldslayer/`)

```text
~/.coldslayer/
├── config.json                  # Watched folders, age thresholds (30d), ignore & purge rules
├── entries.json                 # Atomic metadata & byte-offset store
├── tantivy_index/               # Embedded Tantivy (Lucene in Rust) BM25 + Fuzzy inverted index
├── CATALOG.md                   # Centralized AI-Agent-Readable Master Manifest
├── catalogs/                    # Detailed per-directory Markdown manifests
│   └── users-karan-downloads-212ccfbc.md
└── packs/                       # Single merged seekable Zstandard pack per watched directory
    └── users-karan-downloads-212ccfbc.pack.zst
```

---

## License & Attribution

Copyright (C) 2026 **Karan (`xlogn`)**.

Licensed under the **[GNU Affero General Public License v3.0 (AGPL-3.0)](./LICENSE)**.
* You are free to install, use, study, and contribute to `coldslayer` (`csl`).
* Any redistribution, fork, or derivative work (including network/hosted services) **must retain original copyright and attribution notices**, state any modifications made, and be released under the same **AGPL-3.0** license. Misrepresenting authorship or distributing closed-source/rebranded proprietary versions is strictly prohibited.
