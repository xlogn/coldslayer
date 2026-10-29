---
name: coldslayer
description: >-
  Search, inspect, restore, and archive cold files (files not accessed in the last 30+ days)
  that have been compressed into Zstandard (.zst) packs by Coldslayer (csl). Activate this skill
  whenever a user asks to find missing/old/archived files, clean up old directories to save disk
  space, or inspect ~/.coldslayer/CATALOG.md.
---

# Coldslayer (`csl`) — AI-Native Cold File Archiver & Search Skill

Coldslayer (`csl`, also aliased as `coldslayer`) automatically compresses cold files (default: not accessed in the last 30 days) into single per-folder seekable Zstandard (`.pack.zst`) archives in `~/.coldslayer/packs/` while maintaining:
1. **A local BM25 Inverted Index (`~/.coldslayer/index.db`)** for `<5ms` full-text keyword, symbol, and filename search.
2. **A Centralized Agent-Readable Manifest (`~/.coldslayer/CATALOG.md`)** and per-folder manifests (`~/.coldslayer/catalogs/*.md`) mapping every archived file back to its original directory path.

---

## When to Use Coldslayer (`csl`)

1. **A file the user mentions is missing from the active directory:**
   - Before concluding a file was deleted, search Coldslayer (`csl search "<query>"` or `csl s "<query>"`) or read `~/.coldslayer/CATALOG.md`.
2. **Reading an archived file without cluttering the disk:**
   - Use `csl peek "<path-or-id>"` to stream the file's decompressed content directly to stdout in `<2ms` via byte-offset random access.
3. **Restoring an archived file back to its original location:**
   - Use `csl revive "<path-or-id>"` to restore the file and automatically compact the folder's `.pack.zst`.
4. **Cleaning up / archiving cold files in a folder:**
   - Run `csl slay <folder> --older-than 30d --dry-run` first to preview, then `csl slay <folder> --older-than 30d`.

---

## CLI Command Reference (`csl`)

### 1. Search Cold Storage (BM25 Inverted Index)
```bash
# Search across filenames, extracted keywords, code symbols, headings, and summaries
csl search "payment webhook"
csl s "payment webhook"

# JSON output for programmatic agent parsing
csl s "payment webhook" --json

# Restrict search to a specific original directory or file extension
csl s "auth" --dir ~/Desktop/projects --ext go
```

### 2. Peek at a File (Zero Disk Extraction)
```bash
# Stream decompressed file contents to stdout by ID, filename, or original path
csl peek 12
csl peek "/Users/name/Downloads/design_doc.md"
```

### 3. Restore (Revive) a File or Directory
```bash
# Restore a single file back to its original path (and remove it from cold storage)
csl revive "/Users/name/Downloads/design_doc.md"

# Restore a copy while keeping it archived in the pack
csl revive 12 --keep

# Restore all archived files belonging to a directory
csl revive --dir "/Users/name/Downloads"
```

### 4. Archive (Slay) Cold Files
```bash
# Register a folder to be watched
csl watch ~/Downloads --older-than 30d

# Preview which files haven't been accessed in 30 days
csl slay ~/Downloads --older-than 30d --dry-run

# Compress cold files into ~/.coldslayer/packs/, update index.db & CATALOG.md, and remove originals
csl slay ~/Downloads --older-than 30d

# Sweep all watched folders configured in ~/.coldslayer/config.json
csl slay
```

### 5. Status & Compaction
```bash
# View global space saved, watched directories, and catalog paths
csl status

# Compact all .pack.zst archives so each watched folder has one clean merged file
csl compact
```
