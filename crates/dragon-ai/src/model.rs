//! dragon-ai::model - Creador de embeddings estáticos Model2Vec de 256 dimensiones.
//! Soporta lectura directa de pesos en formato SafeTensors sin frameworks pesados,
//! y fallback determinista por hash de n-gramas.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

pub const EMBEDDING_DIM: usize = 256;
const MAX_HEADER_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_TOKENS: usize = 512;

/// Proveedor de embeddings estáticos (Model2Vec o Fallback n-gram).
pub struct DragonEmbedder {
    pub embeddings: Vec<f32>,
    pub vocab: HashMap<String, u32>,
    pub dim: usize,
    pub unknown_id: u32,
    pub lowercase: bool,
    pub normalize: bool,
    continuing_prefix: String,
    is_real_model: bool,
}

impl DragonEmbedder {
    /// Carga el modelo desde un directorio o busca en rutas del sistema conocidas.
    pub fn load_or_detect<P: AsRef<Path>>(preferred_dir: Option<P>) -> Self {
        // 1. Probar ruta preferida si se especificó
        if let Some(dir) = preferred_dir {
            if let Ok(model) = Self::load_from_dir(dir.as_ref()) {
                log::info!("Modelo Model2Vec cargado desde ruta especificada: {:?}", dir.as_ref());
                return model;
            }
        }

        // 2. Probar ruta de Kensaku existente en AppData
        if let Ok(appdata) = std::env::var("APPDATA") {
            let kensaku_path = Path::new(&appdata).join("kensaku").join("models");
            if let Ok(model) = Self::load_from_dir(&kensaku_path) {
                log::info!("Modelo Model2Vec detectado y cargado desde Kensaku: {:?}", kensaku_path);
                return model;
            }
        }

        // 3. Probar carpeta local .dragon-models
        let local_path = Path::new(".dragon-models");
        if let Ok(model) = Self::load_from_dir(local_path) {
            log::info!("Modelo Model2Vec cargado desde: {:?}", local_path);
            return model;
        }

        // 4. Fallback determinista
        log::warn!("Archivos de modelo Model2Vec no encontrados. Usando HashEmbedder (256 dims)");
        Self::fallback_hash_embedder()
    }

    /// Carga model.safetensors, tokenizer.json y config.json desde una carpeta.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let model_path = dir.join("model.safetensors");
        let tokenizer_path = dir.join("tokenizer.json");
        let config_path = dir.join("config.json");

        let mut file = File::open(&model_path)
            .with_context(|| format!("No se pudo abrir {}", model_path.display()))?;
        let (embeddings, vocab_size, dim) = parse_safetensors(&mut file)
            .with_context(|| "Formato safetensors inválido")?;

        if dim != EMBEDDING_DIM {
            anyhow::bail!("Dimensión del modelo ({}) no coincide con {}", dim, EMBEDDING_DIM);
        }

        let tokenizer_bytes = std::fs::read(&tokenizer_path)
            .with_context(|| format!("No se pudo leer {}", tokenizer_path.display()))?;
        let tokenizer_val: Value = serde_json::from_slice(&tokenizer_bytes)
            .with_context(|| "tokenizer.json contiene JSON inválido")?;

        let (vocab, unknown_id, lowercase, continuing_prefix) = parse_tokenizer(&tokenizer_val, vocab_size)
            .with_context(|| "Vocabulario de tokenizer no coincide con safetensors")?;

        let normalize = if let Ok(config_bytes) = std::fs::read(&config_path) {
            serde_json::from_slice::<Value>(&config_bytes)
                .ok()
                .and_then(|v| v.get("normalize").and_then(|b| b.as_bool()))
                .unwrap_or(true)
        } else {
            true
        };

        Ok(Self {
            embeddings,
            vocab,
            dim,
            unknown_id,
            lowercase,
            normalize,
            continuing_prefix,
            is_real_model: true,
        })
    }

    /// Embedder determinista sin modelo externo.
    pub fn fallback_hash_embedder() -> Self {
        Self {
            embeddings: Vec::new(),
            vocab: HashMap::new(),
            dim: EMBEDDING_DIM,
            unknown_id: 0,
            lowercase: true,
            normalize: true,
            continuing_prefix: "##".to_string(),
            is_real_model: false,
        }
    }

    pub fn is_real_model(&self) -> bool {
        self.is_real_model
    }

    /// Genera un embedding vectorial de dimensión 256 para el texto dado.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dim];
        if text.trim().is_empty() {
            return vec;
        }

        if !self.is_real_model || self.embeddings.is_empty() {
            // Hash embedding rápido sobre n-gramas de caracteres
            self.embed_hash(text, &mut vec);
            if self.normalize {
                l2_normalize(&mut vec);
            }
            return vec;
        }

        // Tokenización y mean-pooling
        let ids = self.tokenize(text);
        if ids.is_empty() {
            return vec;
        }

        let mut count = 0usize;
        for id in ids {
            let start = id as usize * self.dim;
            let end = start + self.dim;
            if end <= self.embeddings.len() {
                for (slot, &val) in vec.iter_mut().zip(&self.embeddings[start..end]) {
                    *slot += val;
                }
                count += 1;
            }
        }

        if count > 0 {
            let inv = 1.0 / count as f32;
            for val in &mut vec {
                *val *= inv;
            }
        }

        if self.normalize {
            l2_normalize(&mut vec);
        }

        vec
    }

    fn embed_hash(&self, text: &str, out: &mut [f32]) {
        let lower = text.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        for word in words {
            let h = hash_str(word);
            let idx = (h as usize) % self.dim;
            out[idx] += 1.0;

            // Subwords (3-gramas)
            for window in word.as_bytes().windows(3) {
                let sub_h = hash_bytes(window);
                let sub_idx = (sub_h as usize) % self.dim;
                out[sub_idx] += 0.5;
            }
        }
    }

    fn tokenize(&self, text: &str) -> Vec<u32> {
        let mut ids = Vec::new();
        let lower = if self.lowercase { text.to_lowercase() } else { text.to_string() };
        for word in lower.split_whitespace() {
            if ids.len() >= MAX_TOKENS {
                break;
            }
            if let Some(&id) = self.vocab.get(word) {
                ids.push(id);
                continue;
            }

            // WordPiece subwords
            let chars: Vec<char> = word.chars().collect();
            let mut start = 0;
            while start < chars.len() {
                let mut end = chars.len();
                let mut matched = None;
                while end > start {
                    let mut piece: String = chars[start..end].iter().collect();
                    if start > 0 {
                        piece.insert_str(0, &self.continuing_prefix);
                    }
                    if let Some(&id) = self.vocab.get(&piece) {
                        matched = Some(id);
                        break;
                    }
                    end -= 1;
                }

                if let Some(id) = matched {
                    ids.push(id);
                    start = end;
                } else {
                    ids.push(self.unknown_id);
                    break;
                }
            }
        }
        ids
    }
}

pub fn l2_normalize(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|&x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        let inv = 1.0 / norm;
        for x in vec.iter_mut() {
            *x *= inv;
        }
    }
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
    }
    dot
}

fn hash_str(s: &str) -> u64 {
    hash_bytes(s.as_bytes())
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn parse_safetensors<R: Read + Seek>(reader: &mut R) -> Option<(Vec<f32>, usize, usize)> {
    let mut length_bytes = [0u8; 8];
    reader.read_exact(&mut length_bytes).ok()?;
    let header_len = u64::from_le_bytes(length_bytes);
    if header_len == 0 || header_len > MAX_HEADER_BYTES {
        return None;
    }
    let mut header_bytes = vec![0u8; header_len as usize];
    reader.read_exact(&mut header_bytes).ok()?;
    let header: Value = serde_json::from_slice(&header_bytes).ok()?;
    let tensors = header.as_object()?;

    let (shape, begin, end) = {
        let mut found = None;
        for (name, tensor) in tensors {
            if name == "__metadata__" {
                continue;
            }
            if let Some(info) = parse_tensor_info(tensor) {
                if info.0.len() == 2 {
                    found = Some(info);
                    break;
                }
            }
        }
        found?
    };

    let vocab_size = shape[0];
    let dim = shape[1];
    let total_elements = vocab_size * dim;

    let data_offset = 8 + header_len;
    reader.seek(std::io::SeekFrom::Start(data_offset + begin)).ok()?;

    let mut buffer = vec![0u8; (end - begin) as usize];
    reader.read_exact(&mut buffer).ok()?;

    let mut embeddings = Vec::with_capacity(total_elements);
    for chunk in buffer.chunks_exact(4) {
        let val = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        embeddings.push(val);
    }

    if embeddings.len() != total_elements {
        return None;
    }

    Some((embeddings, vocab_size, dim))
}

fn parse_tensor_info(val: &Value) -> Option<(Vec<usize>, u64, u64)> {
    let obj = val.as_object()?;
    let shape_arr = obj.get("shape")?.as_array()?;
    let mut shape = Vec::new();
    for v in shape_arr {
        shape.push(v.as_u64()? as usize);
    }
    let offsets = obj.get("data_offsets")?.as_array()?;
    let begin = offsets.get(0)?.as_u64()?;
    let end = offsets.get(1)?.as_u64()?;
    Some((shape, begin, end))
}

fn parse_tokenizer(val: &Value, _expected_vocab_size: usize) -> Option<(HashMap<String, u32>, u32, bool, String)> {
    let model = val.get("model")?.as_object()?;
    let vocab_obj = model.get("vocab")?.as_object()?;
    let mut vocab = HashMap::with_capacity(vocab_obj.len());
    for (k, v) in vocab_obj {
        vocab.insert(k.clone(), v.as_u64()? as u32);
    }

    let unk_token = model.get("unk_token").and_then(|v| v.as_str()).unwrap_or("[UNK]");
    let unknown_id = vocab.get(unk_token).copied().unwrap_or(0);

    let continuing_prefix = model
        .get("continuing_subword_prefix")
        .and_then(|v| v.as_str())
        .unwrap_or("##")
        .to_string();

    let lowercase = val
        .get("normalizer")
        .and_then(|n| n.get("lowercase"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    Some((vocab, unknown_id, lowercase, continuing_prefix))
}
