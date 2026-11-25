//! dragon-ai::vector_store - Almacén vectorial contiguo en memoria con escaneo SIMD y Rayon.
//! Permite persistencia binaria compacta y búsqueda k-NN instantánea.

use crate::model::{cosine_similarity, EMBEDDING_DIM};
use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorUnit {
    pub doc_id: u64,
    pub path: String,
    pub chunk_index: u32,
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct VectorHit {
    pub doc_id: u64,
    pub path: String,
    pub score: f32,
    pub snippet: String,
}

pub struct VectorStore {
    pub units: Vec<VectorUnit>,
    pub vectors: Vec<f32>,
    pub dim: usize,
    path_map: HashMap<String, usize>,
}

impl Default for VectorStore {
    fn default() -> Self {
        Self::new(EMBEDDING_DIM)
    }
}

fn vector_path_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

impl VectorStore {
    pub fn new(dim: usize) -> Self {
        Self {
            units: Vec::new(),
            vectors: Vec::new(),
            dim,
            path_map: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// Recupera en O(1) desde RAM el extracto almacenado de un documento por ruta (0 I/O de disco).
    pub fn get_snippet_by_path(&self, path: &str) -> Option<&str> {
        let key = vector_path_key(path);
        let &idx = self.path_map.get(&key)?;
        self.units.get(idx).map(|u| u.snippet.as_str()).filter(|s| !s.is_empty())
    }

    /// Agrega o actualiza (sin duplicar) una unidad vectorial al almacén según su ruta.
    pub fn upsert(&mut self, doc_id: u64, path: &str, chunk_index: u32, snippet: &str, vector: &[f32]) {
        assert_eq!(vector.len(), self.dim, "Dimensión del vector no coincide");
        let key = vector_path_key(path);

        if let Some(&idx) = self.path_map.get(&key) {
            if idx < self.units.len() {
                self.units[idx].doc_id = doc_id;
                self.units[idx].path = path.to_string();
                self.units[idx].chunk_index = chunk_index;
                self.units[idx].snippet = snippet.to_string();
                let start = idx * self.dim;
                let end = start + self.dim;
                self.vectors[start..end].copy_from_slice(vector);
                return;
            }
        }

        let new_idx = self.units.len();
        self.units.push(VectorUnit {
            doc_id,
            path: path.to_string(),
            chunk_index,
            snippet: snippet.to_string(),
        });
        self.vectors.extend_from_slice(vector);
        self.path_map.insert(key, new_idx);
    }

    /// Agrega una unidad vectorial al almacén (alias de upsert para evitar duplicados).
    pub fn add(&mut self, doc_id: u64, path: &str, chunk_index: u32, snippet: &str, vector: &[f32]) {
        self.upsert(doc_id, path, chunk_index, snippet, vector);
    }

    /// Actualiza la ruta e ID de un vector cuando el archivo se movió de ubicación en disco.
    pub fn relocate_path(&mut self, old_path: &str, new_path: &str, new_doc_id: u64) -> bool {
        let old_key = vector_path_key(old_path);
        let Some(idx) = self.path_map.remove(&old_key) else {
            return false;
        };
        if idx < self.units.len() {
            self.units[idx].path = new_path.to_string();
            self.units[idx].doc_id = new_doc_id;
            let new_key = vector_path_key(new_path);
            self.path_map.insert(new_key, idx);
            true
        } else {
            false
        }
    }

    /// Elimina una unidad vectorial por ruta en tiempo O(1) usando swap-remove.
    pub fn remove_by_path(&mut self, path: &str) -> bool {
        let key = vector_path_key(path);
        let Some(idx) = self.path_map.remove(&key) else {
            return false;
        };
        let last_idx = self.units.len() - 1;
        if idx != last_idx {
            let start = idx * self.dim;
            let last_start = last_idx * self.dim;
            for offset in 0..self.dim {
                self.vectors[start + offset] = self.vectors[last_start + offset];
            }
            self.units.swap_remove(idx);
            let moved_key = vector_path_key(&self.units[idx].path);
            self.path_map.insert(moved_key, idx);
        } else {
            self.units.pop();
        }
        self.vectors.truncate(self.units.len() * self.dim);
        true
    }

    /// Búsqueda k-NN exacta en paralelo mediante Rayon.
    pub fn search(&self, query_vec: &[f32], limit: usize) -> Vec<VectorHit> {
        if self.is_empty() || query_vec.len() != self.dim {
            return Vec::new();
        }

        let dim = self.dim;
        let units = &self.units;
        let vectors = &self.vectors;

        // Escaneo multi-hilo en Rayon
        let scored_indices: Vec<(usize, f32)> = (0..units.len())
            .into_par_iter()
            .map(|idx| {
                let start = idx * dim;
                let end = start + dim;
                let slice = &vectors[start..end];
                let sim = cosine_similarity(query_vec, slice);
                (idx, sim)
            })
            .filter(|&(_, sim)| sim > 0.05)
            .collect();

        // Agrupar por documento (doc_id) manteniendo el mayor score
        let mut doc_best: HashMap<u64, (f32, usize)> = HashMap::new();
        for (idx, sim) in scored_indices {
            let doc_id = units[idx].doc_id;
            doc_best
                .entry(doc_id)
                .and_modify(|existing| {
                    if sim > existing.0 {
                        *existing = (sim, idx);
                    }
                })
                .or_insert((sim, idx));
        }

        let mut hits: Vec<VectorHit> = doc_best
            .into_iter()
            .map(|(doc_id, (score, best_idx))| {
                let unit = &units[best_idx];
                VectorHit {
                    doc_id,
                    path: unit.path.clone(),
                    score,
                    snippet: unit.snippet.clone(),
                }
            })
            .collect();

        // Ordenar descendentemente por score
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        hits
    }

    /// Guarda el almacén en un archivo binario rápido.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        // Header: Magic "DRGV", Version 1, dim, count
        writer.write_all(b"DRGV")?;
        writer.write_all(&1u32.to_le_bytes())?;
        writer.write_all(&(self.dim as u32).to_le_bytes())?;
        writer.write_all(&(self.units.len() as u64).to_le_bytes())?;

        // Serializar unidades en JSON
        let units_json = serde_json::to_vec(&self.units)?;
        writer.write_all(&(units_json.len() as u64).to_le_bytes())?;
        writer.write_all(&units_json)?;

        // Serializar vectores crudos como f32 en Little Endian
        let mut raw_bytes = Vec::with_capacity(self.vectors.len() * 4);
        for &val in &self.vectors {
            raw_bytes.extend_from_slice(&val.to_le_bytes());
        }
        writer.write_all(&raw_bytes)?;
        writer.flush()?;

        Ok(())
    }

    /// Carga el almacén desde un archivo binario.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if &magic != b"DRGV" {
            anyhow::bail!("Firma mágica inválida en archivo vectorial");
        }

        let mut u32_buf = [0u8; 4];
        reader.read_exact(&mut u32_buf)?;
        let _version = u32::from_le_bytes(u32_buf);

        reader.read_exact(&mut u32_buf)?;
        let dim = u32::from_le_bytes(u32_buf) as usize;

        let mut u64_buf = [0u8; 8];
        reader.read_exact(&mut u64_buf)?;
        let total_units = u64::from_le_bytes(u64_buf) as usize;

        reader.read_exact(&mut u64_buf)?;
        let json_len = u64::from_le_bytes(u64_buf) as usize;

        let mut json_bytes = vec![0u8; json_len];
        reader.read_exact(&mut json_bytes)?;
        let units: Vec<VectorUnit> = serde_json::from_slice(&json_bytes)?;

        let total_floats = total_units * dim;
        let mut raw_bytes = vec![0u8; total_floats * 4];
        reader.read_exact(&mut raw_bytes)?;

        let mut vectors = Vec::with_capacity(total_floats);
        for chunk in raw_bytes.chunks_exact(4) {
            let val = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            vectors.push(val);
        }

        // Deduplicar si un archivo previo ya tuviera entradas repetidas, conservando la última
        let mut dedup_units = Vec::with_capacity(units.len());
        let mut dedup_vectors = Vec::with_capacity(vectors.len());
        let mut path_map = HashMap::with_capacity(units.len());

        for (i, unit) in units.into_iter().enumerate() {
            let key = vector_path_key(&unit.path);
            let start = i * dim;
            let end = start + dim;
            let vec_slice = &vectors[start..end];

            if let Some(&existing_idx) = path_map.get(&key) {
                dedup_units[existing_idx] = unit;
                let dst_start = existing_idx * dim;
                let dst_end = dst_start + dim;
                dedup_vectors[dst_start..dst_end].copy_from_slice(vec_slice);
            } else {
                let new_idx = dedup_units.len();
                dedup_units.push(unit);
                dedup_vectors.extend_from_slice(vec_slice);
                path_map.insert(key, new_idx);
            }
        }

        Ok(Self {
            units: dedup_units,
            vectors: dedup_vectors,
            dim,
            path_map,
        })
    }
}
