use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

#[derive(Debug, Clone)]
pub struct Metadata {
    pub category: String,
    pub summary: String,
    pub symbols: Vec<String>,
    pub keywords: Vec<String>,
    pub body_vocab: String,
}

const MAX_SAMPLE_BYTES: usize = 256 * 1024; // Read up to 256KB for full-vocabulary indexing

static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#{1,4}\s+(.+)$").unwrap());
static CODE_SYM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*(?:pub\s+)?(?:export\s+)?(?:async\s+)?(?:func|type|class|def|interface|struct|enum|impl|fn|trait)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});
static JSON_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^\s*"([A-Za-z0-9_\-\.]{2,40})"\s*:"#).unwrap());
static WORD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z][A-Za-z0-9_\-]{2,40}").unwrap());

static STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "the", "and", "for", "that", "this", "with", "from", "are", "was", "were", "have", "has",
        "had", "not", "but", "what", "all", "when", "can", "there", "use", "each", "which", "she",
        "how", "their", "will", "other", "about", "out", "many", "then", "them", "these", "some",
        "her", "would", "make", "like", "him", "into", "time", "hasn", "more", "write", "see",
        "number", "way", "could", "people", "than", "first", "water", "been", "call", "who", "oil",
        "its", "now", "find", "long", "down", "day", "did", "get", "come", "made", "may", "part",
        "return", "import", "package", "const", "var", "let", "func", "function", "class", "true",
        "false", "null", "nil", "none", "void", "int", "string", "bool", "err", "error", "else",
        "while", "switch", "case", "default", "break", "continue", "struct", "interface", "public",
        "private", "protected", "static", "async", "await", "export", "require", "module", "self",
        "todo", "http", "https", "www", "com", "org", "pub", "mut", "impl", "use", "mod",
    ]
    .into_iter()
    .collect()
});

pub fn extract(abs_path: &Path) -> Metadata {
    let ext = abs_path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let category = classify_extension(&ext).to_string();
    let base_name = abs_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = abs_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| base_name.clone());

    let path_tokens = tokenize_identifier(&stem);

    let mut file = match File::open(abs_path) {
        Ok(f) => f,
        Err(_) => {
            let kw = unique_strings(path_tokens, 25);
            return Metadata {
                category,
                summary: format!("Unreadable file ({base_name})"),
                symbols: Vec::new(),
                body_vocab: kw.join(" "),
                keywords: kw,
            };
        }
    };

    let mut buf = vec![0u8; MAX_SAMPLE_BYTES];
    let n = read_up_to(&mut file, &mut buf);
    buf.truncate(n);

    if n == 0 {
        let kw = unique_strings(path_tokens, 25);
        return Metadata {
            category,
            summary: format!("Empty file ({base_name})"),
            symbols: Vec::new(),
            body_vocab: kw.join(" "),
            keywords: kw,
        };
    }

    if is_binary(&buf) || matches!(category.as_str(), "media" | "archive" | "binary") {
        let mut toks = path_tokens;
        if !ext.is_empty() {
            toks.push(ext.clone());
        }
        toks.push(category.clone());
        let kw = unique_strings(toks, 25);
        let ext_upper = if ext.is_empty() {
            "BIN".to_string()
        } else {
            ext.to_uppercase()
        };
        return Metadata {
            category,
            summary: format!("Binary/media file ({ext_upper}) - {base_name}"),
            symbols: Vec::new(),
            body_vocab: kw.join(" "),
            keywords: kw,
        };
    }

    let text = String::from_utf8_lossy(&buf);
    let symbols = extract_symbols(&text, &category);
    let summary = build_summary(&text, &symbols, &base_name);
    let (keywords, body_vocab) = rank_keywords_and_vocab(&text, &path_tokens, &symbols, 25, 4000);

    Metadata {
        category,
        summary,
        symbols,
        keywords,
        body_vocab,
    }
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> usize {
    let mut total = 0;
    while total < buf.len() {
        match file.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => break,
        }
    }
    total
}

fn classify_extension(ext: &str) -> &'static str {
    match ext {
        "go" | "py" | "js" | "ts" | "tsx" | "jsx" | "rs" | "c" | "cpp" | "h" | "hpp" | "java"
        | "kt" | "swift" | "rb" | "php" | "cs" | "sh" | "zsh" | "bash" | "sql" | "lua" | "zig"
        | "scala" | "r" => "code",
        "md" | "mdx" | "txt" | "rst" | "org" | "adoc" | "tex" => "docs",
        "json" | "yaml" | "yml" | "toml" | "ini" | "env" | "conf" | "cfg" | "xml" | "plist" => {
            "config"
        }
        "csv" | "tsv" | "jsonl" | "ndjson" => "data",
        "log" | "out" | "err" => "log",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "mp4" | "mov" | "mp3" | "wav"
        | "flac" => "media",
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "dmg" | "iso" => {
            "archive"
        }
        "pdf" | "doc" | "docx" | "ppt" | "pptx" | "xls" | "xlsx" => "document",
        _ => "file",
    }
}

fn is_binary(sample: &[u8]) -> bool {
    let check_len = sample.len().min(8192);
    sample[..check_len].contains(&0)
}

fn extract_symbols(text: &str, category: &str) -> Vec<String> {
    let mut out = Vec::new();
    if category == "docs" {
        for cap in HEADING_RE.captures_iter(text).take(12) {
            if let Some(m) = cap.get(1) {
                let h = m.as_str().trim();
                if !h.is_empty() {
                    out.push(h.to_string());
                }
            }
        }
    }
    if category == "code" || out.is_empty() {
        for cap in CODE_SYM_RE.captures_iter(text).take(20) {
            if let Some(m) = cap.get(1) {
                let s = m.as_str().trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
        }
    }
    if category == "config" || category == "data" {
        for cap in JSON_KEY_RE.captures_iter(text).take(20) {
            if let Some(m) = cap.get(1) {
                let k = m.as_str().trim();
                if !k.is_empty() {
                    out.push(k.to_string());
                }
            }
        }
    }
    unique_strings(out, 15)
}

fn build_summary(text: &str, symbols: &[String], fallback_name: &str) -> String {
    let mut snippet_parts = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("#!")
            || trimmed == "---"
            || trimmed == "{"
            || trimmed == "}"
        {
            continue;
        }
        let cleaned = trimmed
            .trim_start_matches(['/', '#', '*', ';', '-', ' '])
            .trim();
        if cleaned.len() < 4 {
            continue;
        }
        snippet_parts.push(cleaned.to_string());
        if snippet_parts.join(" · ").len() > 180 || snippet_parts.len() >= 3 {
            break;
        }
    }

    let mut summary = snippet_parts.join(" · ");
    if !symbols.is_empty() {
        let sym_preview = symbols
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if !summary.is_empty() {
            summary = format!("{summary} [Symbols: {sym_preview}]");
        } else {
            summary = format!("Symbols: {sym_preview}");
        }
    }
    if summary.is_empty() {
        summary = format!("Text file ({fallback_name})");
    }
    if summary.chars().count() > 260 {
        let truncated: String = summary.chars().take(257).collect();
        summary = format!("{truncated}...");
    }
    summary
}

fn rank_keywords_and_vocab(
    text: &str,
    path_tokens: &[String],
    symbols: &[String],
    max_keywords: usize,
    max_vocab: usize,
) -> (Vec<String>, String) {
    let mut scores: HashMap<String, usize> = HashMap::new();
    let mut vocab_order: Vec<String> = Vec::new();
    let mut vocab_seen: HashSet<String> = HashSet::new();

    for pt in path_tokens {
        record_token(
            pt.to_lowercase(),
            12,
            max_vocab,
            &mut scores,
            &mut vocab_order,
            &mut vocab_seen,
        );
    }

    for sym in symbols {
        let lower_sym = sym.to_lowercase();
        if is_valid_keyword(&lower_sym)
            && vocab_order.len() < max_vocab
            && vocab_seen.insert(lower_sym.clone())
        {
            vocab_order.push(lower_sym);
        }
        for tok in tokenize_identifier(sym) {
            record_token(
                tok.to_lowercase(),
                7,
                max_vocab,
                &mut scores,
                &mut vocab_order,
                &mut vocab_seen,
            );
        }
    }

    for (i, m) in WORD_RE.find_iter(text).take(12_000).enumerate() {
        let raw = m.as_str();
        let raw_lower = raw.to_lowercase();
        if raw_lower.len() >= 3
            && raw_lower.len() <= 40
            && vocab_order.len() < max_vocab
            && vocab_seen.insert(raw_lower.clone())
        {
            vocab_order.push(raw_lower);
        }
        let weight = if i < 120 { 2 } else { 1 };
        for sub in tokenize_identifier(raw) {
            record_token(
                sub.to_lowercase(),
                weight,
                max_vocab,
                &mut scores,
                &mut vocab_order,
                &mut vocab_seen,
            );
        }
    }

    let mut ranked: Vec<(String, usize)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let keywords: Vec<String> = ranked
        .into_iter()
        .take(max_keywords)
        .map(|(w, _)| w)
        .collect();

    let body_vocab = vocab_order.join(" ");
    (keywords, body_vocab)
}

fn record_token(
    w: String,
    weight: usize,
    max_vocab: usize,
    scores: &mut HashMap<String, usize>,
    vocab_order: &mut Vec<String>,
    vocab_seen: &mut HashSet<String>,
) {
    if !is_valid_keyword(&w) {
        return;
    }
    *scores.entry(w.clone()).or_insert(0) += weight;
    if vocab_order.len() < max_vocab && vocab_seen.insert(w.clone()) {
        vocab_order.push(w);
    }
}

fn is_valid_keyword(w: &str) -> bool {
    if w.len() < 3 || w.len() > 36 {
        return false;
    }
    !STOP_WORDS.contains(w)
}

pub fn tokenize_identifier(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = s.chars().collect();

    for i in 0..chars.len() {
        let c = chars[i];
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        if i > 0
            && c.is_uppercase()
            && (chars[i - 1].is_lowercase()
                || (i + 1 < chars.len() && chars[i + 1].is_lowercase()))
            && !current.is_empty()
        {
            tokens.push(std::mem::take(&mut current));
        }
        for lower in c.to_lowercase() {
            current.push(lower);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn unique_strings(items: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_lowercase()) {
            out.push(trimmed.to_string());
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}
