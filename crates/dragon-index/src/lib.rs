//! dragon-index: Motor de indexación invertido basado en Tantivy.
//! Soporta compresión de bloques con LZ4, ranking BM25, coincidencia tolerante a acentos,
//! expansión por prefijo y deduplicación de rutas reales.

use anyhow::{Context, Result};
use dragon_core::{classify_path, is_noisy_build_or_cache_path, FileCategory};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, PhraseQuery, Query, RegexQuery, TermQuery};
use tantivy::schema::*;
pub use tantivy::IndexWriter;
use tantivy::{Index, IndexReader, ReloadPolicy};

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

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
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

    /// Crea un escritor para indexar documentos (búfer configurable en MB).
    pub fn create_writer(&self, memory_budget_mb: usize) -> Result<IndexWriter> {
        let budget_bytes = memory_budget_mb * 1024 * 1024;
        self.index.writer(budget_bytes).context("Error creando IndexWriter")
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

        let tokens = extract_query_tokens(trimmed);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let searcher = self.reader.searcher();
        let candidate_limit = (limit * 20).max(300);

        let strict_query = self.build_query(&tokens, include_filename, true);
        let mut raw_docs = searcher.search(&strict_query, &TopDocs::with_limit(candidate_limit))?;

        let strict_count = raw_docs.len();
        if strict_count < limit && tokens.len() > 1 {
            let relaxed_query = self.build_query(&tokens, include_filename, false);
            let extra_docs = searcher.search(&relaxed_query, &TopDocs::with_limit(candidate_limit))?;
            // Dar mayor peso a resultados estrictos que cumplieron todos los términos
            for (s, addr) in &mut raw_docs {
                *s *= 2.5;
                let _ = addr;
            }
            raw_docs.extend(extra_docs);
        }

        let q_lower = trimmed.to_lowercase();
        let mut deduped: HashMap<String, SearchHit> = HashMap::new();

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

            // Resolver ruta real en disco para descartar registros obsoletos/borrados y colapsar rutas relativas vs absolutas
            let Some(resolved_path) = resolve_existing_file_path(&raw_path) else {
                continue;
            };
            let dedup_key = if cfg!(windows) {
                resolved_path.to_lowercase()
            } else {
                resolved_path.clone()
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
            if is_noisy_build_or_cache_path(&resolved_path) {
                score *= 0.02;
            }

            let resolved_name = Path::new(&resolved_path)
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
                    path: resolved_path,
                    filename: resolved_name,
                    size,
                    score,
                    snippet: String::new(),
                });
        }

        let mut hits: Vec<SearchHit> = deduped.into_values().collect();
        hits.sort_by(|a, b| {
            let a_noisy = is_noisy_build_or_cache_path(&a.path);
            let b_noisy = is_noisy_build_or_cache_path(&b.path);
            a_noisy
                .cmp(&b_noisy)
                .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    fn build_query(&self, tokens: &[String], include_filename: bool, require_all: bool) -> Box<dyn Query> {
        let mut outer_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let token_occur = if require_all { Occur::Must } else { Occur::Should };

        for tok in tokens {
            let mut per_token: Vec<(Occur, Box<dyn Query>)> = Vec::new();
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

            // Expansión tolerante a acentos y prefijo en contenido (ej. "informacion" -> "información", "contrat" -> "contratos")
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

            outer_clauses.push((token_occur, Box::new(BooleanQuery::new(per_token))));
        }

        // Boost adicional cuando los términos aparecen como frase cercana dentro del documento
        if tokens.len() >= 2 {
            let phrase_terms: Vec<tantivy::Term> = tokens
                .iter()
                .map(|t| tantivy::Term::from_field_text(self.field_content, t))
                .collect();
            let mut phrase_q = PhraseQuery::new(phrase_terms);
            phrase_q.set_slop(2);
            outer_clauses.push((Occur::Should, Box::new(BoostQuery::new(Box::new(phrase_q), 3.5))));
        }

        Box::new(BooleanQuery::new(outer_clauses))
    }
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

fn resolve_existing_file_path(raw_path: &str) -> Option<String> {
    let p = Path::new(raw_path);
    if !p.is_file() {
        return None;
    }
    let abs: PathBuf = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = abs.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
        Some(format!(r"\\{}", stripped))
    } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
        Some(stripped.to_string())
    } else {
        Some(s.into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
