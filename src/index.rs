use crate::config::{human_bytes, Config};
use crate::extractor::tokenize_identifier;
use crate::packer::{compact_pack, ActiveChunk};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, Value, FAST, INDEXED, STORED, TEXT};
use tantivy::{doc, Index, ReloadPolicy, TantivyDocument, Term};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedFile {
    pub id: u64,
    pub abs_path: String,
    pub rel_path: String,
    pub root_dir: String,
    pub folder_slug: String,
    pub file_name: String,
    pub ext: String,
    pub category: String,
    pub summary: String,
    pub symbols: String,
    pub keywords: String,
    #[serde(default)]
    pub body_vocab: String,
    pub original_size: u64,
    pub compressed_size: u64,
    pub pack_path: String,
    pub byte_offset: u64,
    pub sha256: String,
    pub file_mode: u32,
    pub last_accessed: DateTime<Utc>,
    pub last_modified: DateTime<Utc>,
    pub archived_at: DateTime<Utc>,
    #[serde(default)]
    pub score: f32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub matched_snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EntriesData {
    next_id: u64,
    entries: Vec<ArchivedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderStats {
    pub root_dir: String,
    pub folder_slug: String,
    pub pack_path: String,
    pub file_count: usize,
    pub original_size: u64,
    pub compressed_size: u64,
}

pub struct SearchOptions {
    pub query: String,
    pub dir: Option<String>,
    pub ext: Option<String>,
    pub limit: usize,
}

pub struct Store {
    cfg: Config,
    data: EntriesData,
}

struct TantivyHandles {
    index: Index,
    f_id: Field,
    f_file_name: Field,
    f_rel_path: Field,
    f_keywords: Field,
    f_symbols: Field,
    f_summary: Field,
    f_body_vocab: Field,
    f_ext: Field,
    f_category: Field,
}

impl Store {
    pub fn open(cfg: Config) -> Result<Self> {
        let store_path = cfg.entries_store_path();
        let data: EntriesData = if store_path.exists() {
            let raw = fs::read_to_string(&store_path)
                .with_context(|| format!("failed to read {:?}", store_path))?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            EntriesData {
                next_id: 1,
                entries: Vec::new(),
            }
        };

        let store = Self { cfg, data };
        if !store.cfg.tantivy_dir().join("meta.json").exists() && !store.data.entries.is_empty() {
            store.rebuild_tantivy_index()?;
        }
        Ok(store)
    }

    fn save_entries(&self) -> Result<()> {
        let store_path = self.cfg.entries_store_path();
        let tmp_path = store_path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(&self.data)?;
        fs::write(&tmp_path, raw)?;
        fs::rename(&tmp_path, &store_path)?;
        Ok(())
    }

    fn build_schema() -> (Schema, Field, Field, Field, Field, Field, Field, Field, Field, Field) {
        let mut builder = Schema::builder();
        let f_id = builder.add_u64_field("id", INDEXED | STORED | FAST);
        let f_file_name = builder.add_text_field("file_name", TEXT | STORED);
        let f_rel_path = builder.add_text_field("rel_path", TEXT | STORED);
        let f_keywords = builder.add_text_field("keywords", TEXT | STORED);
        let f_symbols = builder.add_text_field("symbols", TEXT | STORED);
        let f_summary = builder.add_text_field("summary", TEXT | STORED);
        let f_body_vocab = builder.add_text_field("body_vocab", TEXT | STORED);
        let f_ext = builder.add_text_field("ext", TEXT | STORED);
        let f_category = builder.add_text_field("category", TEXT | STORED);
        (
            builder.build(),
            f_id,
            f_file_name,
            f_rel_path,
            f_keywords,
            f_symbols,
            f_summary,
            f_body_vocab,
            f_ext,
            f_category,
        )
    }

    fn open_or_create_tantivy(&self) -> Result<TantivyHandles> {
        let (
            schema,
            f_id,
            f_file_name,
            f_rel_path,
            f_keywords,
            f_symbols,
            f_summary,
            f_body_vocab,
            f_ext,
            f_category,
        ) = Self::build_schema();

        let dir = self.cfg.tantivy_dir();
        fs::create_dir_all(&dir)?;

        let index = if dir.join("meta.json").exists() {
            match Index::open_in_dir(&dir) {
                Ok(idx) => idx,
                Err(_) => {
                    let _ = fs::remove_dir_all(&dir);
                    fs::create_dir_all(&dir)?;
                    Index::create_in_dir(&dir, schema)?
                }
            }
        } else {
            Index::create_in_dir(&dir, schema)?
        };

        Ok(TantivyHandles {
            index,
            f_id,
            f_file_name,
            f_rel_path,
            f_keywords,
            f_symbols,
            f_summary,
            f_body_vocab,
            f_ext,
            f_category,
        })
    }

    pub fn rebuild_tantivy_index(&self) -> Result<()> {
        let dir = self.cfg.tantivy_dir();
        if dir.exists() {
            let _ = fs::remove_dir_all(&dir);
        }
        fs::create_dir_all(&dir)?;
        let h = self.open_or_create_tantivy()?;
        let mut writer = h.index.writer(30_000_000)?;

        for e in &self.data.entries {
            let fn_tokens = tokenize_identifier(&e.file_name).join(" ");
            let rel_tokens = tokenize_identifier(&e.rel_path).join(" ");
            writer.add_document(doc!(
                h.f_id => e.id,
                h.f_file_name => format!("{} {}", e.file_name, fn_tokens),
                h.f_rel_path => format!("{} {}", e.rel_path, rel_tokens),
                h.f_keywords => e.keywords.as_str(),
                h.f_symbols => e.symbols.as_str(),
                h.f_summary => e.summary.as_str(),
                h.f_body_vocab => e.body_vocab.as_str(),
                h.f_ext => e.ext.as_str(),
                h.f_category => e.category.as_str(),
            ))?;
        }
        writer.commit()?;
        Ok(())
    }

    pub fn upsert_batch(&mut self, mut batch: Vec<ArchivedFile>) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        if self.data.next_id == 0 {
            self.data.next_id = 1;
        }

        for item in &mut batch {
            self.data.entries.retain(|existing| existing.abs_path != item.abs_path);
            item.id = self.data.next_id;
            self.data.next_id += 1;
            self.data.entries.push(item.clone());
        }

        self.save_entries()?;
        self.rebuild_tantivy_index()?;
        Ok(())
    }

    /// Multi-Tier Tantivy Search Engine:
    /// - Tier 1: Exact phrase + NEAR(slop=2) proximity + all-terms AND query boosting
    /// - Tier 2: BM25 + FuzzyTermQuery (Levenshtein edit distance 1-2 with transposition)
    /// - Tier 3: Trigram & mid-word substring fallback + highlighted snippet generation
    pub fn search(&self, opts: &SearchOptions) -> Result<Vec<ArchivedFile>> {
        let limit = if opts.limit == 0 { 25 } else { opts.limit };
        let raw_query = opts.query.trim();
        if raw_query.is_empty() {
            return Ok(self.list_recent(opts.dir.as_deref(), opts.ext.as_deref(), limit));
        }

        let dir_filter = opts.dir.as_ref().map(|d| {
            fs::canonicalize(d)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| d.clone())
        });
        let ext_filter = opts
            .ext
            .as_ref()
            .map(|e| e.trim_start_matches('.').to_lowercase());

        let query_tokens = tokenize_identifier(raw_query);
        let mut combined_scores: HashMap<u64, f32> = HashMap::new();

        // 1. Run Tantivy BM25 + Phrase/Proximity + Fuzzy Levenshtein Search
        if !query_tokens.is_empty() && !self.data.entries.is_empty() {
            if let Ok(h) = self.open_or_create_tantivy() {
                if let Ok(reader) = h
                    .index
                    .reader_builder()
                    .reload_policy(ReloadPolicy::Manual)
                    .try_into()
                {
                    let searcher = reader.searcher();
                    let weighted_fields = [
                        (h.f_file_name, 10.0f32),
                        (h.f_keywords, 8.0f32),
                        (h.f_symbols, 6.0f32),
                        (h.f_rel_path, 6.0f32),
                        (h.f_summary, 4.0f32),
                        (h.f_body_vocab, 2.5f32),
                    ];

                    let mut root_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

                    // Tier 1A: Phrase / NEAR proximity query (slop = 2) when 2+ query tokens exist
                    if query_tokens.len() >= 2 {
                        for &(field, weight) in &weighted_fields {
                            let terms: Vec<(usize, Term)> = query_tokens
                                .iter()
                                .enumerate()
                                .map(|(i, tok)| (i, Term::from_field_text(field, tok)))
                                .collect();
                            let mut pq = PhraseQuery::new_with_offset(terms);
                            pq.set_slop(2);
                            root_clauses.push((
                                Occur::Should,
                                Box::new(BoostQuery::new(Box::new(pq), weight * 3.0)),
                            ));
                        }

                        // Tier 1B: All-terms-present (AND / Occur::Must) boost
                        for &(field, weight) in &weighted_fields {
                            let must_terms: Vec<(Occur, Box<dyn Query>)> = query_tokens
                                .iter()
                                .map(|tok| {
                                    let term = Term::from_field_text(field, tok);
                                    (
                                        Occur::Must,
                                        Box::new(TermQuery::new(
                                            term,
                                            IndexRecordOption::WithFreqs,
                                        )) as Box<dyn Query>,
                                    )
                                })
                                .collect();
                            root_clauses.push((
                                Occur::Should,
                                Box::new(BoostQuery::new(
                                    Box::new(BooleanQuery::new(must_terms)),
                                    weight * 1.8,
                                )),
                            ));
                        }
                    }

                    // Tier 2: Individual exact term + Fuzzy Levenshtein prefix match
                    for tok in &query_tokens {
                        let edit_dist = if tok.len() >= 7 {
                            2
                        } else if tok.len() >= 4 {
                            1
                        } else {
                            0
                        };

                        for &(field, weight) in &weighted_fields {
                            let term = Term::from_field_text(field, tok);
                            let exact_q = TermQuery::new(term.clone(), IndexRecordOption::WithFreqs);
                            root_clauses.push((
                                Occur::Should,
                                Box::new(BoostQuery::new(Box::new(exact_q), weight)),
                            ));

                            if edit_dist > 0 {
                                let fuzzy_q = FuzzyTermQuery::new_prefix(term, edit_dist, true);
                                root_clauses.push((
                                    Occur::Should,
                                    Box::new(BoostQuery::new(Box::new(fuzzy_q), weight * 0.55)),
                                ));
                            }
                        }
                    }

                    let boolean_query = BooleanQuery::new(root_clauses);
                    if let Ok(top_docs) =
                        searcher.search(&boolean_query, &TopDocs::with_limit(limit * 4))
                    {
                        for (score, doc_address) in top_docs {
                            if let Ok(retrieved) = searcher.doc::<TantivyDocument>(doc_address) {
                                if let Some(id_val) =
                                    retrieved.get_first(h.f_id).and_then(|v| v.as_u64())
                                {
                                    combined_scores.insert(id_val, score);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Parallel Substring, Phrase, and Trigram/Fuzzy Re-Ranker over all candidate entries
        // Ensures mid-word substrings (e.g. "hook" in "webhook") and multi-word proximity get exact boosts
        let raw_lower = raw_query.to_lowercase();
        let entry_matches: Vec<(ArchivedFile, f32)> = self
            .data
            .entries
            .par_iter()
            .filter(|e| matches_filters(e, dir_filter.as_deref(), ext_filter.as_deref()))
            .filter_map(|e| {
                let mut score = combined_scores.get(&e.id).copied().unwrap_or(0.0);
                let bonus = compute_lexical_and_fuzzy_bonus(e, &raw_lower, &query_tokens);
                score += bonus;
                if score <= 0.05 {
                    return None;
                }
                let mut cloned = e.clone();
                cloned.score = (score * 100.0).round() / 100.0;
                cloned.matched_snippet = build_highlighted_snippet(e, &raw_lower, &query_tokens);
                Some((cloned, score))
            })
            .collect();

        let mut results: Vec<ArchivedFile> = entry_matches.into_iter().map(|(e, _)| e).collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.archived_at.cmp(&a.archived_at))
        });
        results.truncate(limit);
        Ok(results)
    }

    pub fn list_recent(
        &self,
        dir_filter: Option<&str>,
        ext_filter: Option<&str>,
        limit: usize,
    ) -> Vec<ArchivedFile> {
        let dir_canon = dir_filter.map(|d| {
            fs::canonicalize(d)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| d.to_string())
        });
        let ext_clean = ext_filter.map(|e| e.trim_start_matches('.').to_lowercase());

        let mut filtered: Vec<ArchivedFile> = self
            .data
            .entries
            .iter()
            .filter(|e| matches_filters(e, dir_canon.as_deref(), ext_clean.as_deref()))
            .cloned()
            .collect();

        filtered.sort_by(|a, b| {
            b.archived_at
                .cmp(&a.archived_at)
                .then_with(|| a.abs_path.cmp(&b.abs_path))
        });
        filtered.truncate(limit.max(1));
        filtered
    }

    pub fn resolve_target(&self, target: &str) -> Result<ArchivedFile> {
        let target = target.trim();
        if let Ok(id) = target.parse::<u64>() {
            if let Some(found) = self.data.entries.iter().find(|e| e.id == id) {
                return Ok(found.clone());
            }
        }

        let abs_target = fs::canonicalize(target)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| target.to_string());

        if let Some(found) = self.data.entries.iter().find(|e| {
            e.abs_path == target
                || e.abs_path == abs_target
                || e.rel_path == target
                || e.file_name == target
        }) {
            return Ok(found.clone());
        }

        if let Some(found) = self
            .data
            .entries
            .iter()
            .find(|e| e.abs_path.ends_with(target))
        {
            return Ok(found.clone());
        }

        Err(anyhow!("no archived file found matching {target:?}"))
    }

    pub fn delete_by_id(&mut self, id: u64) -> Result<()> {
        self.data.entries.retain(|e| e.id != id);
        self.save_entries()?;
        self.rebuild_tantivy_index()?;
        Ok(())
    }

    pub fn compact_folder_pack(&mut self, folder_slug: &str) -> Result<u64> {
        let mut chunks: Vec<ActiveChunk> = self
            .data
            .entries
            .iter()
            .filter(|e| e.folder_slug == folder_slug)
            .map(|e| ActiveChunk {
                entry_id: e.id,
                byte_offset: e.byte_offset,
                compressed_size: e.compressed_size,
                new_byte_offset: e.byte_offset,
            })
            .collect();

        chunks.sort_by_key(|c| c.byte_offset);
        let pack_path = self.cfg.pack_file_path(folder_slug);
        let (updated, reclaimed) = compact_pack(&pack_path, chunks)?;
        if reclaimed == 0 {
            return Ok(0);
        }

        let offset_map: HashMap<u64, u64> = updated
            .into_iter()
            .map(|c| (c.entry_id, c.new_byte_offset))
            .collect();
        for e in &mut self.data.entries {
            if let Some(&new_off) = offset_map.get(&e.id) {
                e.byte_offset = new_off;
            }
        }
        self.save_entries()?;
        Ok(reclaimed)
    }

    pub fn get_folder_stats(&self) -> Vec<FolderStats> {
        let mut map: BTreeMap<String, FolderStats> = BTreeMap::new();
        for e in &self.data.entries {
            let st = map
                .entry(e.folder_slug.clone())
                .or_insert_with(|| FolderStats {
                    root_dir: e.root_dir.clone(),
                    folder_slug: e.folder_slug.clone(),
                    pack_path: e.pack_path.clone(),
                    file_count: 0,
                    original_size: 0,
                    compressed_size: 0,
                });
            st.file_count += 1;
            st.original_size += e.original_size;
            st.compressed_size += e.compressed_size;
        }
        let mut out: Vec<FolderStats> = map.into_values().collect();
        out.sort_by(|a, b| a.root_dir.cmp(&b.root_dir));
        out
    }

    pub fn sync_catalogs(&self) -> Result<()> {
        let stats = self.get_folder_stats();
        let total_files: usize = stats.iter().map(|s| s.file_count).sum();
        let total_orig: u64 = stats.iter().map(|s| s.original_size).sum();
        let total_comp: u64 = stats.iter().map(|s| s.compressed_size).sum();
        let saved_bytes = total_orig.saturating_sub(total_comp);

        let mut master = String::new();
        master.push_str("# Coldslayer Master Cold-Storage Catalog\n\n");
        master.push_str("> **AI Agent Instructions:** This file is automatically maintained by `csl` (`coldslayer`).\n");
        master.push_str("> It indexes cold files that were compressed to save disk space.\n");
        master.push_str("> - Fast BM25 + Fuzzy search: `csl search \"<keywords>\"` (or `csl s \"<keywords>\"`)\n");
        master.push_str("> - Read file to stdout without extracting: `csl peek \"<abs_path_or_id>\"`\n");
        master.push_str("> - Restore file to original path: `csl revive \"<abs_path_or_id>\"`\n\n");

        master.push_str(&format!(
            "## Global Summary\n- **Total Archived Files:** {}\n- **Original Size:** {}\n- **Compressed Size:** {}\n- **Disk Space Reclaimed:** {}\n- **Updated At:** {}\n\n",
            total_files,
            human_bytes(total_orig),
            human_bytes(total_comp),
            human_bytes(saved_bytes),
            Utc::now().to_rfc3339()
        ));

        master.push_str("## Archived Directories Overview\n\n");
        if stats.is_empty() {
            master.push_str("_No files currently archived._\n");
        } else {
            master.push_str("| Original Directory Path | Files | Original | Compressed | Pack File | Detailed Catalog |\n");
            master.push_str("| :--- | ---: | ---: | ---: | :--- | :--- |\n");
            for st in &stats {
                let cat_path = self.cfg.folder_catalog_path(&st.folder_slug);
                master.push_str(&format!(
                    "| `{}` | {} | {} | {} | `{}` | `{}` |\n",
                    st.root_dir,
                    st.file_count,
                    human_bytes(st.original_size),
                    human_bytes(st.compressed_size),
                    st.pack_path,
                    cat_path.display()
                ));
            }
            master.push('\n');
        }

        master.push_str("## Centralized File Manifest\n\n");
        for st in &stats {
            let mut files: Vec<&ArchivedFile> = self
                .data
                .entries
                .iter()
                .filter(|e| e.folder_slug == st.folder_slug)
                .collect();
            files.sort_by(|a, b| {
                a.last_accessed
                    .cmp(&b.last_accessed)
                    .then_with(|| a.rel_path.cmp(&b.rel_path))
            });

            self.write_folder_catalog(st, &files)?;

            let pack_name = Path::new(&st.pack_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| st.pack_path.clone());
            master.push_str(&format!(
                "### Directory: `{}` (Pack: `{}`)\n\n",
                st.root_dir, pack_name
            ));
            master.push_str("| ID | Relative Path | Type | Last Accessed | Keywords | Summary |\n");
            master.push_str("| ---: | :--- | :--- | :--- | :--- | :--- |\n");
            for f in files {
                let kw = sanitize_cell(&truncate_str(&f.keywords, 70));
                let sum = sanitize_cell(&truncate_str(&f.summary, 110));
                master.push_str(&format!(
                    "| {} | `{}` | {} | {} | {} | {} |\n",
                    f.id,
                    f.rel_path,
                    f.category,
                    f.last_accessed.format("%Y-%m-%d"),
                    kw,
                    sum
                ));
            }
            master.push('\n');
        }

        fs::write(self.cfg.master_catalog_path(), master)?;
        Ok(())
    }

    fn write_folder_catalog(&self, st: &FolderStats, files: &[&ArchivedFile]) -> Result<()> {
        let mut b = String::new();
        b.push_str(&format!(
            "# Coldslayer Directory Catalog: `{}`\n\n",
            st.root_dir
        ));
        b.push_str(&format!("- **Original Directory:** `{}`\n", st.root_dir));
        b.push_str(&format!("- **Merged Zstd Pack:** `{}`\n", st.pack_path));
        b.push_str(&format!(
            "- **Archived Files:** {} ({} -> {})\n\n",
            st.file_count,
            human_bytes(st.original_size),
            human_bytes(st.compressed_size)
        ));

        for f in files {
            b.push_str(&format!("## [{}] `{}`\n", f.id, f.rel_path));
            b.push_str(&format!("- **Original Path:** `{}`\n", f.abs_path));
            b.push_str(&format!(
                "- **Category / Ext:** `{}` (`.{}`)\n",
                f.category, f.ext
            ));
            b.push_str(&format!(
                "- **Size:** {} (compressed: {} at offset `{}`)\n",
                human_bytes(f.original_size),
                human_bytes(f.compressed_size),
                f.byte_offset
            ));
            b.push_str(&format!(
                "- **Last Accessed:** {} | **Archived At:** {}\n",
                f.last_accessed.format("%Y-%m-%d %H:%M"),
                f.archived_at.format("%Y-%m-%d %H:%M")
            ));
            if !f.symbols.is_empty() {
                b.push_str(&format!("- **Symbols / Headings:** {}\n", f.symbols));
            }
            if !f.keywords.is_empty() {
                b.push_str(&format!("- **Keywords:** {}\n", f.keywords));
            }
            b.push_str(&format!("- **Summary:** {}\n\n", f.summary));
        }

        fs::write(self.cfg.folder_catalog_path(&st.folder_slug), b)?;
        Ok(())
    }
}

fn matches_filters(e: &ArchivedFile, dir_filter: Option<&str>, ext_filter: Option<&str>) -> bool {
    if let Some(df) = dir_filter {
        if e.root_dir != df && !e.abs_path.starts_with(df) {
            return false;
        }
    }
    if let Some(ef) = ext_filter {
        if e.ext.to_lowercase() != ef {
            return false;
        }
    }
    true
}

/// Computes phrase proximity, all-terms AND bonus, mid-word substring bonus, and trigram/Levenshtein fuzzy bonus.
fn compute_lexical_and_fuzzy_bonus(
    e: &ArchivedFile,
    raw_lower: &str,
    query_tokens: &[String],
) -> f32 {
    let fn_lower = e.file_name.to_lowercase();
    let rel_lower = e.rel_path.to_lowercase();
    let kw_lower = e.keywords.to_lowercase();
    let sym_lower = e.symbols.to_lowercase();
    let sum_lower = e.summary.to_lowercase();
    let vocab_lower = e.body_vocab.to_lowercase();

    let mut bonus = 0.0f32;

    // Exact phrase / substring match across fields
    if !raw_lower.is_empty() {
        if fn_lower.contains(raw_lower) {
            bonus += 25.0;
        }
        if sym_lower.contains(raw_lower) {
            bonus += 18.0;
        }
        if kw_lower.contains(raw_lower) {
            bonus += 15.0;
        }
        if sum_lower.contains(raw_lower) {
            bonus += 12.0;
        }
        if rel_lower.contains(raw_lower) {
            bonus += 10.0;
        }
        if vocab_lower.contains(raw_lower) {
            bonus += 8.0;
        }
    }

    if query_tokens.is_empty() {
        return bonus;
    }

    let doc_words: HashSet<&str> = vocab_lower
        .split_whitespace()
        .chain(fn_lower.split(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();

    let mut matched_token_count = 0usize;
    for qtok in query_tokens {
        let mut tok_hit = false;
        if fn_lower.contains(qtok) {
            bonus += 8.0;
            tok_hit = true;
        }
        if sym_lower.contains(qtok) {
            bonus += 6.0;
            tok_hit = true;
        }
        if kw_lower.contains(qtok) {
            bonus += 5.0;
            tok_hit = true;
        }
        if sum_lower.contains(qtok) {
            bonus += 3.5;
            tok_hit = true;
        }
        if vocab_lower.contains(qtok) {
            bonus += 2.5;
            tok_hit = true;
        }

        // Fuzzy typo check via Levenshtein edit distance if not an exact substring hit
        if !tok_hit && qtok.len() >= 4 {
            let max_dist = if qtok.len() >= 7 { 2 } else { 1 };
            for &dw in &doc_words {
                if dw.len().abs_diff(qtok.len()) <= max_dist {
                    let dist = levenshtein_distance(qtok, dw);
                    if dist <= max_dist {
                        bonus += if dist == 1 { 4.5 } else { 2.5 };
                        tok_hit = true;
                        break;
                    }
                }
            }
        }

        if tok_hit {
            matched_token_count += 1;
        }
    }

    // All-terms AND boost: if every query token matched (exact, substring, or fuzzy), boost strongly!
    if query_tokens.len() >= 2 && matched_token_count == query_tokens.len() {
        bonus += 18.0;
    }

    bonus
}

/// Generates a highlighted match snippet (`>>match<<`) showing why the file matched.
fn build_highlighted_snippet(
    e: &ArchivedFile,
    raw_lower: &str,
    query_tokens: &[String],
) -> String {
    let mut candidate = e.summary.clone();
    let sum_lower = candidate.to_lowercase();

    // If the match occurred in body_vocab or symbols rather than summary, append matching vocab terms
    let mut matched_vocab_terms = Vec::new();
    for word in e.body_vocab.split_whitespace() {
        let wl = word.to_lowercase();
        let is_hit = (!raw_lower.is_empty() && wl.contains(raw_lower))
            || query_tokens.iter().any(|qt| {
                wl.contains(qt)
                    || (qt.len() >= 4
                        && wl.len().abs_diff(qt.len()) <= 2
                        && levenshtein_distance(qt, &wl) <= if qt.len() >= 7 { 2 } else { 1 })
            });
        if is_hit && !sum_lower.contains(&wl) && matched_vocab_terms.len() < 5 {
            matched_vocab_terms.push(format!(">>{word}<<"));
        }
    }

    for qt in query_tokens {
        if qt.len() >= 2 {
            candidate = highlight_case_insensitive(&candidate, qt);
        }
    }

    if !matched_vocab_terms.is_empty() {
        format!(
            "{candidate}  (Matched tokens: {})",
            matched_vocab_terms.join(", ")
        )
    } else {
        candidate
    }
}

fn highlight_case_insensitive(text: &str, needle: &str) -> String {
    let lower_text = text.to_lowercase();
    let lower_needle = needle.to_lowercase();
    if let Some(pos) = lower_text.find(&lower_needle) {
        let end = pos + lower_needle.len();
        if text.is_char_boundary(pos) && text.is_char_boundary(end) {
            return format!("{}>>{}<<{}", &text[..pos], &text[pos..end], &text[end..]);
        }
    }
    text.to_string()
}

pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

fn sanitize_cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ").trim().to_string()
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{truncated}...")
}
