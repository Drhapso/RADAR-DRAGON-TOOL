//! dragon-ai::hybrid - Fusión de rankings híbridos mediante Reciprocal Rank Fusion (RRF).
//! Combina el índice invertido BM25 (Tantivy) con la similitud semántica (Model2Vec).

use crate::vector_store::VectorHit;
use dragon_index::SearchHit;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct HybridHit {
    pub path: String,
    pub filename: String,
    pub rrf_score: f32,
    pub bm25_score: f32,
    pub semantic_score: f32,
    pub snippet: String,
}

pub struct HybridRanker;

impl HybridRanker {
    /// Combina resultados BM25 y semánticos vectoriales mediante RRF con k=60.
    pub fn fuse(
        bm25_hits: &[SearchHit],
        semantic_hits: &[VectorHit],
        bm25_weight: f32,
        semantic_weight: f32,
        limit: usize,
    ) -> Vec<HybridHit> {
        const K: f32 = 60.0;
        let mut map: HashMap<String, (String, f32, f32, f32, String)> = HashMap::new();

        // 1. Procesar hits BM25
        for (rank, hit) in bm25_hits.iter().enumerate() {
            let rrf_inc = bm25_weight / (K + (rank + 1) as f32);
            map.entry(hit.path.clone())
                .and_modify(|entry| {
                    entry.1 += rrf_inc;
                    entry.2 = entry.2.max(hit.score);
                })
                .or_insert_with(|| {
                    (
                        hit.filename.clone(),
                        rrf_inc,
                        hit.score,
                        0.0,
                        String::new(),
                    )
                });
        }

        // 2. Procesar hits semánticos
        for (rank, hit) in semantic_hits.iter().enumerate() {
            let rrf_inc = semantic_weight / (K + (rank + 1) as f32);
            let filename = std::path::Path::new(&hit.path)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| hit.path.clone());

            map.entry(hit.path.clone())
                .and_modify(|entry| {
                    entry.1 += rrf_inc;
                    entry.3 = entry.3.max(hit.score);
                    if entry.4.is_empty() {
                        entry.4 = hit.snippet.clone();
                    }
                })
                .or_insert_with(|| {
                    (
                        filename,
                        rrf_inc,
                        0.0,
                        hit.score,
                        hit.snippet.clone(),
                    )
                });
        }

        // 3. Convertir a lista y ordenar por RRF descendente
        let mut results: Vec<HybridHit> = map
            .into_iter()
            .map(|(path, (filename, rrf_score, bm25_score, semantic_score, snippet))| {
                HybridHit {
                    path,
                    filename,
                    rrf_score,
                    bm25_score,
                    semantic_score,
                    snippet,
                }
            })
            .collect();

        results.sort_by(|a, b| b.rrf_score.partial_cmp(&a.rrf_score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        results
    }
}
