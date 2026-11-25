//! dragon-index: Motor de indexación invertido basado en Tantivy.
//! Soporta compresión de bloques con LZ4, ranking BM25 y trigramas
//! para búsquedas instantáneas dentro del contenido de documentos.

use anyhow::{Context, Result};
use dragon_core::{classify_path, is_noisy_build_or_cache_path, FileCategory};
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
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
    pub score: f32,
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

    /// Ejecuta una consulta de alta precisión sobre documentos usando BM25 + conjunción por defecto + boost de nombre.
    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let trimmed = query_str.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let searcher = self.reader.searcher();
        let mut query_parser = QueryParser::for_index(
            &self.index,
            vec![self.field_filename, self.field_content],
        );
        query_parser.set_field_boost(self.field_filename, 4.0);
        query_parser.set_field_boost(self.field_content, 1.0);
        query_parser.set_conjunction_by_default();

        let (query, _errors) = query_parser.parse_query_lenient(trimmed);
        let candidate_limit = (limit * 4).max(40);
        let mut top_docs = searcher.search(&query, &TopDocs::with_limit(candidate_limit))?;

        // Si una consulta multi-palabra con AND estricto no arroja resultados, relajar a OR ponderado
        if top_docs.is_empty() && trimmed.split_whitespace().count() > 1 {
            let mut relaxed_parser = QueryParser::for_index(
                &self.index,
                vec![self.field_filename, self.field_content],
            );
            relaxed_parser.set_field_boost(self.field_filename, 4.0);
            let (relaxed_q, _) = relaxed_parser.parse_query_lenient(trimmed);
            top_docs = searcher.search(&relaxed_q, &TopDocs::with_limit(candidate_limit))?;
        }

        let q_lower = trimmed.to_lowercase();
        let mut hits = Vec::with_capacity(top_docs.len());

        for (raw_score, doc_address) in top_docs {
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;

            let id = retrieved_doc.get_first(self.field_id).and_then(|v| v.as_u64()).unwrap_or(0);
            let path = retrieved_doc
                .get_first(self.field_path)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let filename = retrieved_doc
                .get_first(self.field_filename)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Garantizar que el motor de contenidos solo devuelva documentos conocidos
            if classify_path(&path) != FileCategory::Document {
                continue;
            }

            let mut score = raw_score;
            let fn_lower = filename.to_lowercase();
            if fn_lower == q_lower {
                score *= 3.0;
            } else if fn_lower.contains(&q_lower) {
                score *= 1.8;
            }
            if is_noisy_build_or_cache_path(&path) {
                score *= 0.25;
            }

            hits.push(SearchHit {
                id,
                path,
                filename,
                score,
            });
        }

        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        Ok(hits)
    }
}
