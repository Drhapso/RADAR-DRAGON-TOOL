//! dragon-index: Motor de indexación invertido basado en Tantivy.
//! Soporta compresión de bloques con LZ4, ranking BM25, coincidencia tolerante a acentos,
//! expansión por prefijo y deduplicación de rutas reales.

use anyhow::{Context, Result};
use dragon_core::{classify_path, is_noisy_build_or_cache_path, FileCategory};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, PhraseQuery, Query, RegexQuery, TermQuery};
use tantivy::schema::*;
pub use tantivy::IndexWriter;
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyError};

pub struct DragonIndexEngine {
    index: Index,
    reader: IndexReader,
    pub schema: Schema,
    field_id: Field,
    field_path: Field,
    field_filename: Field,
    field_content: Field,
    field_size: Field,
    field_modified: Field,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: u64,
    pub path: String,
    pub filename: String,
    pub size: u64,
    pub score: f32,
    pub snippet: String,
}

impl DragonIndexEngine {
    /// Abre o crea un índice Tantivy en el directorio especificado.
    pub fn open_or_create<P: AsRef<Path>>(index_dir: P) -> Result<Self> {
        let path = index_dir.as_ref();
        std::fs::create_dir_all(path)?;
        clear_readonly_flags_in_dir(path);

        let mut schema_builder = Schema::builder();
        let field_id = schema_builder.add_u64_field("id", STORED | INDEXED | FAST);
        let field_path = schema_builder.add_text_field("path", STORED | STRING);
        let field_filename = schema_builder.add_text_field("filename", STORED | TEXT);
        let field_content = schema_builder.add_text_field("content", TEXT);
        let field_size = schema_builder.add_u64_field("size", STORED | FAST);
        let field_modified = schema_builder.add_u64_field("modified", STORED | FAST);
        let schema = schema_builder.build();

        let index = Index::open_or_create(tantivy::directory::MmapDirectory::open(path)?, schema.clone())
            .context("Error abriendo índice Tantivy")?;

        // Usamos ReloadPolicy::Manual porque el indexador recarga explícitamente tras cada commit.
        // Evita que un watcher en segundo plano bloquee `meta.json` en Windows (os error 5) durante commit().
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;

        Ok(Self {
            index,
            reader,
            schema,
            field_id,
            field_path,
            field_filename,
            field_content,
            field_size,
            field_modified,
        })
    }

    /// Crea un escritor para indexar documentos (búfer configurable en MB) con reintentos ante
    /// bloqueos transitorios de Windows (antivirus / os error 5 o 32).
    pub fn create_writer(&self, memory_budget_mb: usize) -> Result<IndexWriter> {
        let budget_bytes = memory_budget_mb * 1024 * 1024;
        let max_attempts = 6usize;
        let mut last_err = None;
        for attempt in 0..max_attempts {
            match self.index.writer(budget_bytes) {
                Ok(writer) => return Ok(writer),
                Err(err) => {
                    let retryable = is_transient_windows_io_error(&err)
                        || matches!(err, TantivyError::LockFailure(..));
                    if retryable && attempt + 1 < max_attempts {
                        let wait_ms = 120 * (attempt as u64 + 1);
                        log::warn!(
                            "Bloqueo transitorio al crear IndexWriter (intento {}/{}): {}; reintentando en {} ms",
                            attempt + 1,
                            max_attempts,
                            err,
                            wait_ms
                        );
                        std::thread::sleep(Duration::from_millis(wait_ms));
                        last_err = Some(err);
                        continue;
                    }
                    return Err(err).context("Error creando IndexWriter");
                }
            }
        }
        Err(last_err.unwrap()).context("Error creando IndexWriter tras reintentos")
    }

    /// Ejecuta `writer.commit()` con reintentos exponenciales ante bloqueos transitorios de Windows
    /// (`os error 5` Acceso denegado / `os error 32` archivo en uso por Defender o mmap) y recarga el lector.
    pub fn commit_with_retry(&self, writer: &mut IndexWriter) -> Result<u64> {
        let max_attempts = 8usize;
        for attempt in 0..max_attempts {
            match writer.commit() {
                Ok(opstamp) => {
                    let _ = self.reload();
                    return Ok(opstamp);
                }
                Err(err) => {
                    if is_transient_windows_io_error(&err) && attempt + 1 < max_attempts {
                        let wait_ms = (80u64 << attempt.min(4)).min(1000);
                        log::warn!(
                            "Bloqueo transitorio en commit de Tantivy (intento {}/{}): {}; reintentando en {} ms",
                            attempt + 1,
                            max_attempts,
                            err,
                            wait_ms
                        );
                        std::thread::sleep(Duration::from_millis(wait_ms));
                        continue;
                    }
                    return Err(err).context("Error confirmando cambios en índice Tantivy");
                }
            }
        }
        anyhow::bail!("No se pudo confirmar el índice Tantivy tras reintentos")
    }

    /// Fuerza la recarga del lector para hacer visibles de inmediato los últimos commits.
    pub fn reload(&self) -> Result<()> {
        self.reader.reload().context("Error recargando IndexReader")
    }

    /// Elimina del índice cualquier documento previo con esta ruta exacta.
    pub fn delete_by_path(&self, writer: &mut IndexWriter, path: &str) {
        let term = tantivy::Term::from_field_text(self.field_path, path);
        writer.delete_term(term);
    }

    /// Añade o reemplaza un documento en el índice (evitando duplicados por ruta).
    pub fn add_document(
        &self,
        writer: &mut IndexWriter,
        id: u64,
        path: &str,
        filename: &str,
        content: &str,
        size: u64,
        modified: u64,
    ) -> Result<()> {
        self.delete_by_path(writer, path);

        let mut doc = TantivyDocument::default();
        doc.add_u64(self.field_id, id);
        doc.add_text(self.field_path, path);
        doc.add_text(self.field_filename, filename);
        if !content.is_empty() {
            doc.add_text(self.field_content, content);
        }
        doc.add_u64(self.field_size, size);
        doc.add_u64(self.field_modified, modified);

        writer.add_document(doc)?;
        Ok(())
    }

    /// Busca documentos por contenido y nombre (priorizando el contenido real y evitando ruido).
    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_internal(query_str, true, limit)
    }

    /// Busca estrictamente dentro del campo de contenido extraído de los documentos.
    pub fn search_content(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_internal(query_str, false, limit)
    }

    fn search_internal(
        &self,
        query_str: &str,
        include_filename: bool,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let trimmed = query_str.trim();
        if trimmed.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let parsed = parse_content_query(trimmed);
        if parsed.is_empty() {
            return Ok(Vec::new());
        }

        let searcher = self.reader.searcher();
        let total_docs = (searcher.num_docs() as usize).max(1);
        let candidate_limit = if limit >= total_docs {
            total_docs
        } else {
            limit.saturating_mul(12).max(200).min(total_docs)
        };

        let strict_query = self.build_query(&parsed, include_filename, true);
        let mut raw_docs = searcher.search(&strict_query, &TopDocs::with_limit(candidate_limit))?;

        // Precisión estilo Kensaku: solo activar respaldo relajado si NO hubo ninguna coincidencia estricta
        // y la consulta tiene 3+ palabras sin frases exactas entre comillas (evita falsos positivos en búsquedas de 2 términos).
        if raw_docs.is_empty() && parsed.exact_phrases.is_empty() && parsed.positive_tokens.len() >= 3 {
            let relaxed_query = self.build_query(&parsed, include_filename, false);
            let mut extra_docs = searcher.search(&relaxed_query, &TopDocs::with_limit(candidate_limit))?;
            for (s, _) in &mut extra_docs {
                *s *= 0.25;
            }
            raw_docs.extend(extra_docs);
        }

        let q_lower = trimmed.to_lowercase();
        let mut deduped: HashMap<String, SearchHit> = HashMap::new();

        // Paso 1: deduplicar y puntuar 100% en memoria SIN syscalls de disco (0 `canonicalize` en esta fase)
        for (raw_score, doc_address) in raw_docs {
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;

            let id = retrieved_doc.get_first(self.field_id).and_then(|v| v.as_u64()).unwrap_or(0);
            let raw_path = retrieved_doc
                .get_first(self.field_path)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let filename = retrieved_doc
                .get_first(self.field_filename)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let size = retrieved_doc.get_first(self.field_size).and_then(|v| v.as_u64()).unwrap_or(0);

            if raw_path.is_empty() || classify_path(&raw_path) != FileCategory::Document {
                continue;
            }

            let dedup_key = if cfg!(windows) {
                raw_path.to_lowercase()
            } else {
                raw_path.clone()
            };

            let mut score = raw_score;
            if include_filename {
                let fn_lower = filename.to_lowercase();
                if fn_lower == q_lower {
                    score *= 2.4;
                } else if fn_lower.contains(&q_lower) {
                    score *= 1.5;
                }
            }
            if is_noisy_build_or_cache_path(&raw_path) {
                score *= 0.02;
            }

            let resolved_name = Path::new(&raw_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or(filename);

            deduped
                .entry(dedup_key)
                .and_modify(|existing| {
                    if score > existing.score {
                        existing.score = score;
                    }
                })
                .or_insert_with(|| SearchHit {
                    id,
                    path: raw_path,
                    filename: resolved_name,
                    size,
                    score,
                    snippet: String::new(),
                });
        }

        // Paso 2: ordenar por relevancia y validar existencia en disco SOLO hasta completar `limit`
        let mut candidates: Vec<SearchHit> = deduped.into_values().collect();
        candidates.sort_by(|a, b| {
            let a_noisy = is_noisy_build_or_cache_path(&a.path);
            let b_noisy = is_noisy_build_or_cache_path(&b.path);
            a_noisy
                .cmp(&b_noisy)
                .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
        });

        let mut hits: Vec<SearchHit> = Vec::with_capacity(limit.min(candidates.len()));
        for hit in candidates {
            if fast_file_exists(&hit.path) {
                hits.push(hit);
                if hits.len() >= limit {
                    break;
                }
            }
        }
        Ok(hits)
    }

    fn build_query(
        &self,
        parsed: &ParsedContentQuery,
        include_filename: bool,
        require_all: bool,
    ) -> Box<dyn Query> {
        let mut outer_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let token_occur = if require_all { Occur::Must } else { Occur::Should };

        // 1. Frases exactas entre comillas ("orden del dia") -> Occur::Must con PhraseQuery
        for phrase in &parsed.exact_phrases {
            if phrase.len() == 1 {
                let mut per_tok = Vec::new();
                self.push_token_clauses(&mut per_tok, &phrase[0], include_filename);
                outer_clauses.push((Occur::Must, Box::new(BooleanQuery::new(per_tok))));
            } else if phrase.len() >= 2 {
                let phrase_terms: Vec<tantivy::Term> = phrase
                    .iter()
                    .map(|t| tantivy::Term::from_field_text(self.field_content, t))
                    .collect();
                let mut pq = PhraseQuery::new(phrase_terms);
                pq.set_slop(0);
                let mut phrase_alts: Vec<(Occur, Box<dyn Query>)> = vec![(
                    Occur::Should,
                    Box::new(BoostQuery::new(Box::new(pq), 4.5)),
                )];
                // También tolerar versión sin tildes si difiere
                let folded_terms: Vec<String> = phrase.iter().map(|t| fold_accents(t)).collect();
                if folded_terms != *phrase {
                    let ft: Vec<tantivy::Term> = folded_terms
                        .iter()
                        .map(|t| tantivy::Term::from_field_text(self.field_content, t))
                        .collect();
                    let mut fpq = PhraseQuery::new(ft);
                    fpq.set_slop(0);
                    phrase_alts.push((Occur::Should, Box::new(BoostQuery::new(Box::new(fpq), 4.0))));
                }
                outer_clauses.push((Occur::Must, Box::new(BooleanQuery::new(phrase_alts))));
            }
        }

        // 2. Términos positivos (tolerantes a acentos y prefijo)
        for tok in &parsed.positive_tokens {
            let mut per_token: Vec<(Occur, Box<dyn Query>)> = Vec::new();
            self.push_token_clauses(&mut per_token, tok, include_filename);
            outer_clauses.push((token_occur, Box::new(BooleanQuery::new(per_token))));
        }

        // 3. Exclusiones explícitas (!termino / -termino) -> Occur::MustNot
        for ex in &parsed.excluded_tokens {
            let folded = fold_accents(ex);
            let t1 = tantivy::Term::from_field_text(self.field_content, ex);
            outer_clauses.push((
                Occur::MustNot,
                Box::new(TermQuery::new(t1, IndexRecordOption::Basic)),
            ));
            if folded != *ex {
                let t2 = tantivy::Term::from_field_text(self.field_content, &folded);
                outer_clauses.push((
                    Occur::MustNot,
                    Box::new(TermQuery::new(t2, IndexRecordOption::Basic)),
                ));
            }
            if has_foldable_latin_char(&folded) {
                let pat = build_accent_regex_pattern(&folded, false);
                if let Ok(rx) = RegexQuery::from_pattern(&pat, self.field_content) {
                    outer_clauses.push((Occur::MustNot, Box::new(rx)));
                }
            }
        }

        // 4. Boost adicional cuando múltiples términos aparecen como frase cercana dentro del documento
        if parsed.positive_tokens.len() >= 2 {
            let phrase_terms: Vec<tantivy::Term> = parsed
                .positive_tokens
                .iter()
                .map(|t| tantivy::Term::from_field_text(self.field_content, t))
                .collect();
            let mut phrase_q = PhraseQuery::new(phrase_terms);
            phrase_q.set_slop(2);
            outer_clauses.push((Occur::Should, Box::new(BoostQuery::new(Box::new(phrase_q), 3.5))));
        }

        Box::new(BooleanQuery::new(outer_clauses))
    }

    fn push_token_clauses(
        &self,
        per_token: &mut Vec<(Occur, Box<dyn Query>)>,
        tok: &str,
        include_filename: bool,
    ) {
        let folded = fold_accents(tok);
        let char_len = tok.chars().count();

        // Coincidencia exacta en contenido (máximo peso)
        let content_term = tantivy::Term::from_field_text(self.field_content, tok);
        per_token.push((
            Occur::Should,
            Box::new(BoostQuery::new(
                Box::new(TermQuery::new(content_term, IndexRecordOption::WithFreqsAndPositions)),
                3.0,
            )),
        ));

        if folded != *tok {
            let folded_term = tantivy::Term::from_field_text(self.field_content, &folded);
            per_token.push((
                Occur::Should,
                Box::new(BoostQuery::new(
                    Box::new(TermQuery::new(folded_term, IndexRecordOption::WithFreqsAndPositions)),
                    2.7,
                )),
            ));
        }

        // Expansión tolerante a acentos y prefijo en contenido
        let allow_prefix = char_len >= 3;
        if allow_prefix || has_foldable_latin_char(&folded) {
            let pattern = build_accent_regex_pattern(&folded, allow_prefix);
            if let Ok(rx) = RegexQuery::from_pattern(&pattern, self.field_content) {
                per_token.push((Occur::Should, Box::new(BoostQuery::new(Box::new(rx), 2.5))));
            }
        }

        if include_filename {
            let fn_term = tantivy::Term::from_field_text(self.field_filename, tok);
            per_token.push((
                Occur::Should,
                Box::new(BoostQuery::new(
                    Box::new(TermQuery::new(fn_term, IndexRecordOption::WithFreqsAndPositions)),
                    1.8,
                )),
            ));
            if folded != *tok {
                let fn_folded = tantivy::Term::from_field_text(self.field_filename, &folded);
                per_token.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(
                        Box::new(TermQuery::new(fn_folded, IndexRecordOption::WithFreqsAndPositions)),
                        1.6,
                    )),
                ));
            }
            if allow_prefix || has_foldable_latin_char(&folded) {
                let pattern = build_accent_regex_pattern(&folded, allow_prefix);
                if let Ok(rx) = RegexQuery::from_pattern(&pattern, self.field_filename) {
                    per_token.push((Occur::Should, Box::new(BoostQuery::new(Box::new(rx), 1.2))));
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct ParsedContentQuery {
    positive_tokens: Vec<String>,
    exact_phrases: Vec<Vec<String>>,
    excluded_tokens: Vec<String>,
}

impl ParsedContentQuery {
    fn is_empty(&self) -> bool {
        self.positive_tokens.is_empty() && self.exact_phrases.is_empty()
    }
}

fn parse_content_query(query: &str) -> ParsedContentQuery {
    let mut parsed = ParsedContentQuery::default();
    let mut chars = query.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() || ch == '|' {
            chars.next();
            continue;
        }
        if ch == '"' {
            chars.next();
            let mut phrase_raw = String::new();
            while let Some(c) = chars.next() {
                if c == '"' {
                    break;
                }
                phrase_raw.push(c);
            }
            let toks = extract_query_tokens(&phrase_raw);
            if !toks.is_empty() {
                parsed.exact_phrases.push(toks);
            }
            continue;
        }
        let mut word = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() || c == '"' || c == '|' {
                break;
            }
            word.push(c);
            chars.next();
        }
        let lower = word.to_ascii_lowercase();
        // Omitir modificadores exclusivos de ruta (path:, folder:, d:\...) en búsqueda de contenido
        if lower.starts_with("path:")
            || lower.starts_with("ruta:")
            || lower.starts_with("folder:")
            || lower.starts_with("carpeta:")
            || (word.len() >= 2 && word.as_bytes()[0].is_ascii_alphabetic() && word.as_bytes()[1] == b':')
        {
            continue;
        }
        if let Some(rest) = word.strip_prefix('!').or_else(|| word.strip_prefix('-')) {
            if !rest.is_empty() {
                for tok in extract_query_tokens(rest) {
                    parsed.excluded_tokens.push(tok);
                }
                continue;
            }
        }
        for tok in extract_query_tokens(&word) {
            parsed.positive_tokens.push(tok);
        }
    }
    parsed
}

fn fast_file_exists(raw_path: &str) -> bool {
    let p = Path::new(raw_path);
    if p.is_file() {
        return true;
    }
    if cfg!(windows) && raw_path.len() >= 248 && !raw_path.starts_with(r"\\?\") {
        let extended = if let Some(unc) = raw_path.strip_prefix(r"\\") {
            PathBuf::from(format!(r"\\?\UNC\{}", unc))
        } else {
            PathBuf::from(format!(r"\\?\{}", raw_path))
        };
        return extended.is_file();
    }
    false
}

fn extract_query_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in query.chars() {
        if ch.is_alphanumeric() {
            for lc in ch.to_lowercase() {
                current.push(lc);
            }
        } else if !current.is_empty() {
            if current.len() <= 40 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if !current.is_empty() && current.len() <= 40 {
        tokens.push(current);
    }
    tokens
}

fn fold_accents(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let mapped = match ch {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            _ => ch,
        };
        out.push(mapped);
    }
    out
}

fn has_foldable_latin_char(s: &str) -> bool {
    s.chars().any(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'n' | 'c'))
}

fn build_accent_regex_pattern(folded_tok: &str, allow_prefix: bool) -> String {
    let mut pat = String::with_capacity(folded_tok.len() * 3 + 4);
    for ch in folded_tok.chars() {
        match ch {
            'a' => pat.push_str("[aáàäâã]"),
            'e' => pat.push_str("[eéèëê]"),
            'i' => pat.push_str("[iíìïî]"),
            'o' => pat.push_str("[oóòöôõ]"),
            'u' => pat.push_str("[uúùüû]"),
            'n' => pat.push_str("[nñ]"),
            'c' => pat.push_str("[cç]"),
            c if c.is_alphanumeric() => pat.push(c),
            _ => {}
        }
    }
    if allow_prefix {
        pat.push_str(".*");
    }
    pat
}

fn clear_readonly_flags_in_dir(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Ok(meta) = std::fs::metadata(&p) {
                    let mut perms = meta.permissions();
                    if perms.readonly() {
                        perms.set_readonly(false);
                        let _ = std::fs::set_permissions(&p, perms);
                    }
                }
            }
        }
    }
}

fn is_transient_windows_io_error(err: &TantivyError) -> bool {
    match err {
        TantivyError::IoError(io_err) => {
            if io_err.kind() == std::io::ErrorKind::PermissionDenied {
                return true;
            }
            matches!(io_err.raw_os_error(), Some(5) | Some(32) | Some(33))
        }
        _ => {
            let msg = err.to_string().to_ascii_lowercase();
            msg.contains("os error 5")
                || msg.contains("os error 32")
                || msg.contains("os error 33")
                || msg.contains("acceso denegado")
                || msg.contains("access is denied")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_windows_io_error_detects_access_denied() {
        let io_err = std::io::Error::from_raw_os_error(5);
        let tantivy_err = TantivyError::from(io_err);
        assert!(is_transient_windows_io_error(&tantivy_err));
    }

    #[test]
    fn content_search_handles_accents_prefixes_dedup_and_noise() {
        let tmp_dir = std::env::temp_dir().join(format!("dragon_index_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).unwrap();

        let idx_dir = tmp_dir.join("idx");
        let docs_dir = tmp_dir.join("docs");
        let noise_dir = tmp_dir.join("target").join("debug").join(".fingerprint");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::create_dir_all(&noise_dir).unwrap();

        let real_doc = docs_dir.join("acta.txt");
        let noise_doc = noise_dir.join("radar.json");
        std::fs::write(&real_doc, "Información confidencial sobre contratos de arrendamiento.").unwrap();
        std::fs::write(&noise_doc, "informacion contratos radar fingerprint").unwrap();

        let engine = DragonIndexEngine::open_or_create(&idx_dir).unwrap();
        let mut writer = engine.create_writer(16).unwrap();

        let real_str = real_doc.to_string_lossy().to_string();
        let noise_str = noise_doc.to_string_lossy().to_string();

        engine
            .add_document(&mut writer, 1, &real_str, "acta.txt", "Información confidencial sobre contratos de arrendamiento.", 64, 1)
            .unwrap();
        engine
            .add_document(&mut writer, 2, &noise_str, "radar.json", "informacion contratos radar fingerprint", 40, 1)
            .unwrap();
        writer.commit().unwrap();
        engine.reload().unwrap();

        // 1. Consulta sin acento + prefijo ("informacion contrat") debe encontrar "Información ... contratos"
        let hits = engine.search_content("informacion contrat", 10).unwrap();
        assert!(
            hits.iter()
                .any(|h| Path::new(&h.path).file_name().unwrap() == "acta.txt"),
            "Se esperaba encontrar acta.txt con consulta sin acento y prefijo"
        );

        // 2. search_content no debe devolver coincidencias exclusivas del nombre ("acta")
        let name_only = engine.search_content("acta", 10).unwrap();
        assert!(name_only.is_empty());

        // 3. Exclusión estilo Kensaku/Everything (-arrendamiento / !arrendamiento)
        let excluded = engine.search_content("informacion !arrendamiento", 10).unwrap();
        assert!(
            excluded.iter().all(|h| Path::new(&h.path).file_name().unwrap() != "acta.txt"),
            "acta.txt debió excluirse con !arrendamiento"
        );

        // 4. Frase exacta entre comillas ("contratos de arrendamiento")
        let exact = engine.search_content("\"contratos de arrendamiento\"", 10).unwrap();
        assert!(exact.iter().any(|h| Path::new(&h.path).file_name().unwrap() == "acta.txt"));

        // 5. Precisión estricta de 2 palabras: no devolver falsos positivos si falta una palabra
        let strict_miss = engine.search_content("informacion inexistente999", 10).unwrap();
        assert!(strict_miss.is_empty());

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
