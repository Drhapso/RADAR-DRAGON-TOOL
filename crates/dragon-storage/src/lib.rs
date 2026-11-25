//! dragon-storage: Motor de persistencia compacto basado en redb (B-Tree MVCC en Rust).
//! Evita los desbordamientos masivos de archivos WAL que afectan a SQLite en indexadores
//! masivos, garantizando ACID y auto-reclamación eficiente de espacio en disco.

use anyhow::{Context, Result};
use dragon_core::{
    classify_extension, classify_path, extract_extension_lower, is_noisy_build_or_cache_path,
    FileCategory, FileRecord,
};
use rayon::prelude::*;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

// Definición de tablas tipadas
const FILES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("files_metadata");
const PATH_INDEX_TABLE: TableDefinition<&str, u64> = TableDefinition::new("path_to_id");
const EXT_INDEX_TABLE: TableDefinition<(&str, u64), ()> = TableDefinition::new("extension_index");
const SETTINGS_TABLE: TableDefinition<&str, &str> = TableDefinition::new("settings");
const INDEX_STATE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("index_state");
const CHECKPOINT_SETTING_KEY: &str = "indexing_checkpoint";
const EXT_INDEX_VERSION_KEY: &str = "extension_index_version";
const EXT_INDEX_VERSION_VALUE: &str = "1";

/// Estadística agregada de una extensión indexada en Redb.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionStat {
    pub extension: String,
    pub count: usize,
    pub category: FileCategory,
}

/// Huella de un archivo previamente indexado para actualización incremental y detección de movimientos.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexedFileState {
    pub doc_id: u64,
    pub size: u64,
    pub modified_ms: u64,
    pub last_indexed_ms: u64,
    #[serde(default)]
    pub is_dir: bool,
    #[serde(default)]
    pub original_path: String,
    /// Versión del pipeline de extracción de contenido con la que se indexó este registro.
    #[serde(default)]
    pub content_version: u8,
}

/// Punto de control persistente para reanudar escaneos interrumpidos y validar al inicio.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckpointState {
    /// Verdadero una vez completada al menos una indexación integral del equipo.
    pub initial_scan_completed: bool,
    /// Verdadero mientras una sesión de escaneo está activa (permite reanudar si se interrumpe).
    pub in_progress: bool,
    /// Rutas objetivo planificadas para la sesión actual.
    pub targets: Vec<String>,
    /// Índice dentro de `targets` del último punto de control alcanzado.
    pub current_target_idx: usize,
    /// Última ruta o archivo confirmado en el punto de control.
    pub last_checkpoint_path: Option<String>,
    /// Contador acumulado de elementos procesados en la sesión actual.
    pub processed_count: usize,
    /// Marca de tiempo Unix (ms) del último checkpoint guardado.
    pub updated_at_ms: u64,
}

struct CachedFileMeta {
    id: u64,
    parent_id: u64,
    name: String,
    name_folded: String,
    stem_folded: String,
    path_str: String,
    path_folded: String,
    size: u64,
    attributes: u32,
    modified_ms: u64,
    is_directory: bool,
    category: FileCategory,
    noisy_path: bool,
}

impl CachedFileMeta {
    fn from_record(record: &FileRecord) -> Self {
        let path_str = record
            .path
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let category = if record.is_directory {
            FileCategory::SystemOrBinary
        } else {
            classify_path(&record.name)
        };
        let name_folded = fold_search_text(&record.name);
        let stem_folded = Path::new(&record.name)
            .file_stem()
            .map(|s| fold_search_text(&s.to_string_lossy()))
            .unwrap_or_else(|| name_folded.clone());
        let path_folded = fold_search_text(&path_str);
        let noisy_path = is_noisy_build_or_cache_path(&path_folded);
        Self {
            id: record.id,
            parent_id: record.parent_id,
            name: record.name.clone(),
            name_folded,
            stem_folded,
            path_str,
            path_folded,
            size: record.size,
            attributes: record.attributes,
            modified_ms: record.modified_ms,
            is_directory: record.is_directory,
            category,
            noisy_path,
        }
    }

    fn to_record(&self) -> FileRecord {
        FileRecord {
            id: self.id,
            parent_id: self.parent_id,
            name: self.name.clone(),
            path: if self.path_str.is_empty() {
                None
            } else {
                Some(PathBuf::from(&self.path_str))
            },
            size: self.size,
            attributes: self.attributes,
            modified_ms: self.modified_ms,
            is_directory: self.is_directory,
        }
    }
}

struct CachedNameIndex {
    entries: Vec<CachedFileMeta>,
    by_id: HashMap<u64, usize>,
}

impl CachedNameIndex {
    fn upsert(&mut self, meta: CachedFileMeta) {
        if let Some(&idx) = self.by_id.get(&meta.id) {
            self.entries[idx] = meta;
        } else {
            let idx = self.entries.len();
            self.by_id.insert(meta.id, idx);
            self.entries.push(meta);
        }
    }

    fn remove_id(&mut self, id: u64) {
        let Some(idx) = self.by_id.remove(&id) else {
            return;
        };
        let last_idx = self.entries.len().saturating_sub(1);
        if idx != last_idx {
            self.entries.swap_remove(idx);
            let moved_id = self.entries[idx].id;
            self.by_id.insert(moved_id, idx);
        } else {
            self.entries.pop();
        }
    }
}

pub struct DragonStorage {
    db: Database,
    name_cache: RwLock<Option<CachedNameIndex>>,
}

impl DragonStorage {
    /// Abre o inicializa la base de datos embebida en un único archivo.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db_path = path.as_ref();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let db = Database::builder()
            .set_cache_size(128 * 1024 * 1024)
            .create(db_path)
            .context("Error abriendo la base de datos redb de Dragon Radar")?;

        // Inicializar tablas si no existen
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(FILES_TABLE)?;
            let _ = write_txn.open_table(PATH_INDEX_TABLE)?;
            let _ = write_txn.open_table(EXT_INDEX_TABLE)?;
            let _ = write_txn.open_table(SETTINGS_TABLE)?;
            let _ = write_txn.open_table(INDEX_STATE_TABLE)?;
        }
        write_txn.commit()?;

        let storage = Self {
            db,
            name_cache: RwLock::new(None),
        };
        let needs_ext_backfill = storage
            .get_setting(EXT_INDEX_VERSION_KEY)?
            .as_deref()
            != Some(EXT_INDEX_VERSION_VALUE);
        if needs_ext_backfill {
            let indexed = storage.rebuild_extension_index()?;
            if indexed > 0 {
                log::info!("Índice de extensiones inicializado para {} archivos existentes", indexed);
            }
        }

        Ok(storage)
    }

    /// Reconstruye el índice invertido de extensiones a partir de los metadatos existentes en `files_metadata`.
    pub fn rebuild_extension_index(&self) -> Result<usize> {
        let write_txn = self.db.begin_write()?;
        let mut count = 0usize;
        {
            let files_table = write_txn.open_table(FILES_TABLE)?;
            let mut ext_table = write_txn.open_table(EXT_INDEX_TABLE)?;
            let mut settings_table = write_txn.open_table(SETTINGS_TABLE)?;

            // Limpiar entradas previas si existían
            let existing_keys: Vec<(String, u64)> = ext_table
                .iter()?
                .filter_map(|res| res.ok().map(|(k, _)| {
                    let (ext, id) = k.value();
                    (ext.to_string(), id)
                }))
                .collect();
            for (ext, id) in existing_keys {
                let _ = ext_table.remove((ext.as_str(), id))?;
            }

            for item in files_table.iter()? {
                let (_, val) = item?;
                if let Ok(record) = serde_json::from_slice::<FileRecord>(val.value()) {
                    if !record.is_directory {
                        if let Some(ext) = extract_extension_lower(&record.name) {
                            ext_table.insert((ext.as_str(), record.id), ())?;
                            count += 1;
                        }
                    }
                }
            }

            settings_table.insert(EXT_INDEX_VERSION_KEY, EXT_INDEX_VERSION_VALUE)?;
        }
        write_txn.commit()?;
        Ok(count)
    }

    /// Inserta o actualiza un lote de registros de archivos en una sola transacción atómica.
    pub fn insert_batch(&self, records: &[FileRecord]) -> Result<usize> {
        let write_txn = self.db.begin_write()?;
        let mut count = 0;

        {
            let mut files_table = write_txn.open_table(FILES_TABLE)?;
            let mut path_table = write_txn.open_table(PATH_INDEX_TABLE)?;
            let mut ext_table = write_txn.open_table(EXT_INDEX_TABLE)?;

            for record in records {
                let new_ext = if record.is_directory {
                    None
                } else {
                    extract_extension_lower(&record.name)
                };

                if let Some(prev_val) = files_table.get(record.id)? {
                    if let Ok(prev_rec) = serde_json::from_slice::<FileRecord>(prev_val.value()) {
                        if !prev_rec.is_directory {
                            if let Some(old_ext) = extract_extension_lower(&prev_rec.name) {
                                if new_ext.as_deref() != Some(old_ext.as_str()) {
                                    let _ = ext_table.remove((old_ext.as_str(), record.id))?;
                                }
                            }
                        }
                    }
                }

                let serialized = serde_json::to_vec(record)?;
                files_table.insert(record.id, serialized.as_slice())?;

                if let Some(ref ext) = new_ext {
                    ext_table.insert((ext.as_str(), record.id), ())?;
                }

                if let Some(ref path) = record.path {
                    if let Some(path_str) = path.to_str() {
                        path_table.insert(path_str, record.id)?;
                    }
                }
                count += 1;
            }
        }

        write_txn.commit()?;
        // Mantener sincronizada la caché en RAM si ya fue inicializada (actualización incremental O(batch))
        if let Ok(mut guard) = self.name_cache.write() {
            if let Some(ref mut idx) = *guard {
                for record in records {
                    idx.upsert(CachedFileMeta::from_record(record));
                }
            }
        }
        Ok(count)
    }

    /// Precarga en RAM el índice de nombres/rutas normalizados para búsquedas en ~10 ms sin I/O de disco.
    pub fn warm_name_cache(&self) -> Result<usize> {
        {
            if let Ok(guard) = self.name_cache.read() {
                if let Some(ref idx) = *guard {
                    return Ok(idx.entries.len());
                }
            }
        }
        let read_txn = self.db.begin_read()?;
        let files_table = read_txn.open_table(FILES_TABLE)?;
        let cap = files_table.len().unwrap_or(0) as usize;
        let mut entries = Vec::with_capacity(cap);
        let mut by_id = HashMap::with_capacity(cap);

        for item in files_table.iter()? {
            let (_, val) = item?;
            if let Ok(record) = serde_json::from_slice::<FileRecord>(val.value()) {
                let meta = CachedFileMeta::from_record(&record);
                let idx = entries.len();
                by_id.insert(meta.id, idx);
                entries.push(meta);
            }
        }
        let total = entries.len();
        if let Ok(mut guard) = self.name_cache.write() {
            *guard = Some(CachedNameIndex { entries, by_id });
        }
        Ok(total)
    }

    /// Recupera un registro por su ID MFT.
    pub fn get_by_id(&self, id: u64) -> Result<Option<FileRecord>> {
        let read_txn = self.db.begin_read()?;
        let files_table = read_txn.open_table(FILES_TABLE)?;

        if let Some(val) = files_table.get(id)? {
            let record: FileRecord = serde_json::from_slice(val.value())?;
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    /// Recupera el total de archivos indexados en la base de datos.
    pub fn count_records(&self) -> Result<usize> {
        let read_txn = self.db.begin_read()?;
        let files_table = read_txn.open_table(FILES_TABLE)?;
        Ok(files_table.len()? as usize)
    }

    /// Busca registros de archivos por coincidencia de nombre ordenada por precisión.
    pub fn search_by_name(&self, query: &str, limit: usize) -> Result<Vec<FileRecord>> {
        let ranked = self.search_by_name_ranked(query, None, limit)?;
        Ok(ranked.into_iter().map(|(rec, _)| rec).collect())
    }

    /// Búsqueda estilo Everything de alta precisión por nombre y ruta con paralelismo Rayon en RAM (~15 ms).
    /// Soporta: insensibilidad a tildes/diacríticos, frases exactas `"..."`, comodines `*` y `?`,
    /// exclusiones `!termino` / `-termino`, operador OR `a | b` y filtros de ruta/unidad (`d:\2025`, `path:...`).
    pub fn search_by_name_ranked(
        &self,
        query: &str,
        category_filter: Option<FileCategory>,
        limit: usize,
    ) -> Result<Vec<(FileRecord, f32)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let parsed = parse_everything_query(query);
        if parsed.is_empty() {
            return Ok(Vec::new());
        }

        // Asegurar caché en RAM cargada una sola vez
        let _ = self.warm_name_cache()?;
        let guard = self
            .name_cache
            .read()
            .map_err(|_| anyhow::anyhow!("Bloqueo de caché en RAM interrumpido"))?;
        let Some(ref idx) = *guard else {
            return Ok(Vec::new());
        };

        let mut scored: Vec<(usize, f32)> = idx
            .entries
            .par_iter()
            .enumerate()
            .filter_map(|(entry_idx, entry)| {
                if entry.is_directory && category_filter.is_some() {
                    return None;
                }
                if let Some(expected_cat) = category_filter {
                    if entry.category != expected_cat {
                        return None;
                    }
                }
                score_entry_everything(entry, &parsed).map(|sc| (entry_idx, sc))
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        let out = scored
            .into_iter()
            .map(|(entry_idx, sc)| (idx.entries[entry_idx].to_record(), sc))
            .collect();
        Ok(out)
    }

    /// Verifica en O(log N) si existe al menos un archivo indexado con la extensión exacta indicada.
    pub fn extension_exists(&self, ext: &str) -> Result<bool> {
        let clean = normalize_extension_token(ext);
        if clean.is_empty() {
            return Ok(false);
        }
        let read_txn = self.db.begin_read()?;
        let ext_table = read_txn.open_table(EXT_INDEX_TABLE)?;
        let mut iter = ext_table.range((clean.as_str(), 0u64)..=(clean.as_str(), u64::MAX))?;
        Ok(iter.next().transpose()?.is_some())
    }

    /// Lista las extensiones más frecuentes presentes en `extension_index` (con filtro opcional por prefijo).
    pub fn list_top_extensions(
        &self,
        prefix_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ExtensionStat>> {
        let prefix = prefix_filter
            .map(normalize_extension_token)
            .filter(|s| !s.is_empty());

        let read_txn = self.db.begin_read()?;
        let ext_table = read_txn.open_table(EXT_INDEX_TABLE)?;

        let mut counts: Vec<ExtensionStat> = Vec::new();
        let mut current_ext = String::new();
        let mut current_count = 0usize;

        let push_current = |counts: &mut Vec<ExtensionStat>, ext: &str, cnt: usize| {
            if !ext.is_empty() && cnt > 0 {
                counts.push(ExtensionStat {
                    extension: ext.to_string(),
                    count: cnt,
                    category: classify_extension(ext),
                });
            }
        };

        if let Some(ref pref) = prefix {
            for item in ext_table.range((pref.as_str(), 0u64)..)? {
                let (k, _) = item?;
                let (ext, _) = k.value();
                if !ext.starts_with(pref.as_str()) {
                    break;
                }
                if ext == current_ext {
                    current_count += 1;
                } else {
                    push_current(&mut counts, &current_ext, current_count);
                    current_ext.clear();
                    current_ext.push_str(ext);
                    current_count = 1;
                }
            }
        } else {
            for item in ext_table.iter()? {
                let (k, _) = item?;
                let (ext, _) = k.value();
                if ext == current_ext {
                    current_count += 1;
                } else {
                    push_current(&mut counts, &current_ext, current_count);
                    current_ext.clear();
                    current_ext.push_str(ext);
                    current_count = 1;
                }
            }
        }
        push_current(&mut counts, &current_ext, current_count);

        counts.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.extension.cmp(&b.extension))
        });
        counts.truncate(limit);
        Ok(counts)
    }

    /// Busca archivos indexados directamente por su extensión mediante el índice B-Tree `extension_index`.
    /// Opcionalmente combina tokens de nombre/ruta y filtros por categoría.
    pub fn search_by_extension(
        &self,
        extensions: &[String],
        name_tokens: &[String],
        category_filter: Option<FileCategory>,
        allow_prefix_fallback: bool,
        limit: usize,
    ) -> Result<Vec<(FileRecord, f32)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut clean_exts: Vec<String> = Vec::new();
        for ext in extensions {
            let norm = normalize_extension_token(ext);
            if !norm.is_empty() && !clean_exts.contains(&norm) {
                clean_exts.push(norm);
            }
        }
        if clean_exts.is_empty() {
            return Ok(Vec::new());
        }

        let norm_tokens: Vec<String> = name_tokens
            .iter()
            .flat_map(|t| tokenize_search_folded(&fold_search_text(t)))
            .collect();
        let compact_query = compact_alnum(&norm_tokens.join(""));

        let read_txn = self.db.begin_read()?;
        let ext_table = read_txn.open_table(EXT_INDEX_TABLE)?;
        let files_table = read_txn.open_table(FILES_TABLE)?;

        // Cuando hay tokens de texto (ej. ".pdf resolucion") o se solicita expansión sin límite
        // (`limit == usize::MAX`), inspeccionar TODOS los archivos de la extensión.
        let max_inspect_per_ext: usize = if norm_tokens.is_empty() && limit < usize::MAX {
            50_000
        } else {
            usize::MAX
        };
        let mut seen_ids: HashSet<u64> = HashSet::new();
        let mut scored: Vec<(FileRecord, f32)> = Vec::new();

        for target_ext in &clean_exts {
            let mut matched_entries: Vec<(u64, bool)> = Vec::new();

            for item in ext_table.range((target_ext.as_str(), 0u64)..=(target_ext.as_str(), u64::MAX))? {
                let (k, _) = item?;
                let (_, doc_id) = k.value();
                if seen_ids.insert(doc_id) {
                    matched_entries.push((doc_id, true));
                    if matched_entries.len() >= max_inspect_per_ext {
                        break;
                    }
                }
            }

            if matched_entries.is_empty() && allow_prefix_fallback {
                for item in ext_table.range((target_ext.as_str(), 0u64)..)? {
                    let (k, _) = item?;
                    let (found_ext, doc_id) = k.value();
                    if !found_ext.starts_with(target_ext.as_str()) {
                        break;
                    }
                    if seen_ids.insert(doc_id) {
                        matched_entries.push((doc_id, found_ext == target_ext));
                        if matched_entries.len() >= max_inspect_per_ext {
                            break;
                        }
                    }
                }
            }

            for (doc_id, is_exact_ext) in matched_entries {
                let Some(val) = files_table.get(doc_id)? else {
                    continue;
                };
                let Ok(record) = serde_json::from_slice::<FileRecord>(val.value()) else {
                    continue;
                };
                if record.is_directory {
                    continue;
                }

                let cat = classify_path(&record.name);
                if let Some(expected_cat) = category_filter {
                    if cat != expected_cat {
                        continue;
                    }
                }

                let name_folded = fold_search_text(&record.name);
                let path_str = record
                    .path
                    .as_deref()
                    .map(|p| p.to_string_lossy())
                    .unwrap_or_default();
                let path_folded = fold_search_text(&path_str);

                let mut matched_in_name = false;
                if !norm_tokens.is_empty() {
                    let all_in_name = norm_tokens.iter().all(|t| name_folded.contains(t.as_str()));
                    let compact_in_name = !all_in_name
                        && compact_query.len() >= 4
                        && compact_alnum(&name_folded).contains(&compact_query);
                    matched_in_name = all_in_name || compact_in_name;
                    let all_in_path =
                        matched_in_name || norm_tokens.iter().all(|t| path_folded.contains(t.as_str()));
                    if !all_in_path {
                        continue;
                    }
                }

                let mut score = if is_exact_ext { 1000.0 } else { 650.0 };
                if !norm_tokens.is_empty() {
                    score += if matched_in_name { 260.0 } else { 90.0 };
                    for tok in &norm_tokens {
                        if is_word_boundary_match(&name_folded, tok) {
                            score += 80.0;
                        }
                    }
                }

                score += match cat {
                    FileCategory::Document => 60.0,
                    FileCategory::ImageOrDesign => 55.0,
                    FileCategory::Media => 50.0,
                    FileCategory::SystemOrBinary => 0.0,
                };

                if is_noisy_build_or_cache_path(&path_folded) {
                    score -= 220.0;
                }

                score -= (name_folded.len() as f32) * 0.25;
                scored.push((record, score));
            }
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    /// Guarda una configuración o estado clave-valor.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(SETTINGS_TABLE)?;
            table.insert(key, value)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Lee una configuración o estado.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SETTINGS_TABLE)?;
        if let Some(val) = table.get(key)? {
            Ok(Some(val.value().to_string()))
        } else {
            Ok(None)
        }
    }

    /// Carga en memoria el mapa completo de estados indexados (ruta normalizada -> tamaño + fecha).
    pub fn load_index_states(&self) -> Result<HashMap<String, IndexedFileState>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(INDEX_STATE_TABLE)?;
        let mut map = HashMap::with_capacity(table.len().unwrap_or(0) as usize);

        for item in table.iter()? {
            let (k, v) = item?;
            if let Ok(state) = serde_json::from_slice::<IndexedFileState>(v.value()) {
                map.insert(k.value().to_string(), state);
            }
        }
        Ok(map)
    }

    /// Inserta o actualiza en lote las huellas de archivos indexados (indexación incremental).
    pub fn upsert_index_states(&self, states: &[(String, IndexedFileState)]) -> Result<usize> {
        if states.is_empty() {
            return Ok(0);
        }
        let write_txn = self.db.begin_write()?;
        let mut count = 0;
        {
            let mut table = write_txn.open_table(INDEX_STATE_TABLE)?;
            for (path_key, state) in states {
                let bytes = serde_json::to_vec(state)?;
                table.insert(path_key.as_str(), bytes.as_slice())?;
                count += 1;
            }
        }
        write_txn.commit()?;
        Ok(count)
    }

    /// Elimina registros y huellas de archivos borrados del disco.
    pub fn remove_by_paths(&self, items: &[(String, String, u64)]) -> Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        let write_txn = self.db.begin_write()?;
        let mut removed = 0;
        {
            let mut files_table = write_txn.open_table(FILES_TABLE)?;
            let mut path_table = write_txn.open_table(PATH_INDEX_TABLE)?;
            let mut ext_table = write_txn.open_table(EXT_INDEX_TABLE)?;
            let mut state_table = write_txn.open_table(INDEX_STATE_TABLE)?;

            for (path_key, original_path, doc_id) in items {
                if let Some(val) = files_table.get(*doc_id)? {
                    if let Ok(rec) = serde_json::from_slice::<FileRecord>(val.value()) {
                        if !rec.is_directory {
                            if let Some(ext) = extract_extension_lower(&rec.name) {
                                let _ = ext_table.remove((ext.as_str(), *doc_id))?;
                            }
                        }
                    }
                }
                let _ = state_table.remove(path_key.as_str())?;
                let _ = path_table.remove(original_path.as_str())?;
                let _ = files_table.remove(*doc_id)?;
                removed += 1;
            }
        }
        write_txn.commit()?;
        if let Ok(mut guard) = self.name_cache.write() {
            if let Some(ref mut idx) = *guard {
                for (_, _, doc_id) in items {
                    idx.remove_id(*doc_id);
                }
            }
        }
        Ok(removed)
    }

    /// Retorna la cantidad de archivos rastreados en la base de datos incremental.
    pub fn count_index_states(&self) -> Result<usize> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(INDEX_STATE_TABLE)?;
        Ok(table.len()? as usize)
    }

    /// Carga el punto de control (checkpoint) de indexación persistente.
    pub fn load_checkpoint(&self) -> Result<CheckpointState> {
        if let Some(json_str) = self.get_setting(CHECKPOINT_SETTING_KEY)? {
            let cp: CheckpointState = serde_json::from_str(&json_str)
                .context("Error deserializando CheckpointState")?;
            Ok(cp)
        } else {
            Ok(CheckpointState::default())
        }
    }

    /// Persiste el punto de control (checkpoint) actual en una transacción atómica.
    pub fn save_checkpoint(&self, cp: &CheckpointState) -> Result<()> {
        let json_str = serde_json::to_string(cp)?;
        self.set_setting(CHECKPOINT_SETTING_KEY, &json_str)
    }
}

fn normalize_extension_token(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('*')
        .trim_start_matches('.')
        .to_ascii_lowercase()
}

/// Normaliza texto de búsqueda a minúsculas sin tildes ni diacríticos latinos.
fn fold_search_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars().flat_map(|c| c.to_lowercase()) {
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

fn tokenize_search_folded(folded: &str) -> Vec<String> {
    folded
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '\\' | '-' | '_'))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

fn compact_alnum(folded: &str) -> String {
    folded.chars().filter(|c| c.is_alphanumeric()).collect()
}

fn is_word_boundary_match(haystack: &str, needle: &str) -> bool {
    if let Some(pos) = haystack.find(needle) {
        if pos == 0 {
            return true;
        }
        if let Some(prev) = haystack[..pos].chars().next_back() {
            return matches!(prev, ' ' | '_' | '-' | '.' | '(' | '[' | '/' | '\\');
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AtomKind {
    Token,
    Phrase,
    Wildcard,
}

#[derive(Debug, Clone)]
struct QueryAtom {
    text: String,
    kind: AtomKind,
    path_only: bool,
    anchor_end: bool,
}

#[derive(Debug, Default)]
struct ParsedEverythingQuery {
    /// Grupos unidos por AND; dentro de cada grupo los átomos se evalúan con OR (`a | b`).
    and_groups: Vec<Vec<QueryAtom>>,
    /// Exclusiones estilo Everything (`!termino` o `-termino`).
    excludes: Vec<QueryAtom>,
    /// Consulta positiva plegada completa para bonificación por coincidencia exacta/prefijo.
    positive_folded: String,
    /// Versión alfanumérica compacta (ej. "contrato2026" -> coincide con "contrato_2026.pdf").
    positive_compact: String,
    /// Lista ordenada de términos positivos planos para bonificar coincidencia en orden.
    ordered_terms: Vec<String>,
}

impl ParsedEverythingQuery {
    fn is_empty(&self) -> bool {
        self.and_groups.is_empty()
    }
}

fn split_raw_query_lexemes(query: &str) -> Vec<String> {
    let mut lexemes = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in query.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push('"');
            }
            '|' if !in_quotes => {
                if !current.trim().is_empty() {
                    lexemes.push(std::mem::take(&mut current));
                }
                lexemes.push("|".to_string());
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.trim().is_empty() {
                    lexemes.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        lexemes.push(current);
    }
    lexemes
}

fn build_atoms_from_lexeme(raw: &str) -> (bool, Vec<QueryAtom>) {
    let mut s = raw.trim();
    if s.is_empty() {
        return (false, Vec::new());
    }

    let mut negated = false;
    if let Some(rest) = s.strip_prefix('!') {
        if !rest.is_empty() {
            negated = true;
            s = rest;
        }
    } else if let Some(rest) = s.strip_prefix('-') {
        if rest.starts_with('"') || rest.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '*' || c == '.') {
            negated = true;
            s = rest;
        }
    }

    let mut path_only = false;
    let lower_prefix = s.to_ascii_lowercase();
    for pref in ["path:", "ruta:", "folder:", "carpeta:", "dir:"] {
        if lower_prefix.starts_with(pref) {
            s = &s[pref.len()..];
            path_only = true;
            break;
        }
    }

    let is_quoted = s.len() >= 2 && s.starts_with('"') && s.ends_with('"');
    if is_quoted {
        s = &s[1..s.len() - 1];
    } else {
        s = s.trim_matches('"');
    }

    let folded = fold_search_text(s.trim()).replace('/', "\\");
    if folded.is_empty() {
        return (negated, Vec::new());
    }

    // Detectar prefijo de unidad o ruta explícita (ej. "d:\2025", "d:", "c:\users")
    let bytes = folded.as_bytes();
    let looks_like_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if looks_like_drive || folded.contains('\\') {
        path_only = true;
    }

    if is_quoted {
        return (
            negated,
            vec![QueryAtom {
                text: folded,
                kind: AtomKind::Phrase,
                path_only,
                anchor_end: false,
            }],
        );
    }

    if folded.contains('*') || folded.contains('?') {
        let anchor_end = folded.starts_with("*.") && !folded.ends_with('*') && !folded.ends_with('?');
        return (
            negated,
            vec![QueryAtom {
                text: folded,
                kind: AtomKind::Wildcard,
                path_only,
                anchor_end,
            }],
        );
    }

    if path_only {
        return (
            negated,
            vec![QueryAtom {
                text: folded,
                kind: AtomKind::Token,
                path_only: true,
                anchor_end: false,
            }],
        );
    }

    // Separar por `-` y `_` para tolerar diferencias entre "radar-dragon", "radar_dragon" y "radar dragon"
    let sub_parts: Vec<QueryAtom> = folded
        .split(|c: char| matches!(c, '-' | '_'))
        .filter(|p| !p.is_empty())
        .map(|p| QueryAtom {
            text: p.to_string(),
            kind: AtomKind::Token,
            path_only: false,
            anchor_end: false,
        })
        .collect();

    if sub_parts.is_empty() {
        (negated, Vec::new())
    } else {
        (negated, sub_parts)
    }
}

fn parse_everything_query(query: &str) -> ParsedEverythingQuery {
    let lexemes = split_raw_query_lexemes(query);
    let mut parsed = ParsedEverythingQuery::default();
    let mut next_is_or = false;
    let mut positive_display_parts: Vec<String> = Vec::new();

    for lex in lexemes {
        if lex == "|" {
            if !parsed.and_groups.is_empty() {
                next_is_or = true;
            }
            continue;
        }
        let (negated, atoms) = build_atoms_from_lexeme(&lex);
        if atoms.is_empty() {
            continue;
        }
        if negated {
            parsed.excludes.extend(atoms);
            next_is_or = false;
            continue;
        }

        if next_is_or {
            if let Some(last_group) = parsed.and_groups.last_mut() {
                last_group.extend(atoms);
            }
            next_is_or = false;
        } else {
            for atom in atoms {
                if !atom.path_only && atom.kind != AtomKind::Wildcard {
                    positive_display_parts.push(atom.text.clone());
                    parsed.ordered_terms.push(atom.text.clone());
                }
                parsed.and_groups.push(vec![atom]);
            }
        }
    }

    parsed.positive_folded = positive_display_parts.join(" ");
    parsed.positive_compact = compact_alnum(&parsed.positive_folded);
    parsed
}

/// Coincidencia glob rápida con comodines `*` y `?` sobre cadenas ya plegadas.
fn wildcard_match_glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            pi += 1;
            star_ti = ti;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn atom_matches_text(atom: &QueryAtom, target: &str) -> bool {
    match atom.kind {
        AtomKind::Token | AtomKind::Phrase => target.contains(atom.text.as_str()),
        AtomKind::Wildcard => {
            if atom.anchor_end {
                wildcard_match_glob(&atom.text, target)
            } else {
                let mut pat = String::with_capacity(atom.text.len() + 2);
                if !atom.text.starts_with('*') {
                    pat.push('*');
                }
                pat.push_str(&atom.text);
                if !atom.text.ends_with('*') {
                    pat.push('*');
                }
                wildcard_match_glob(&pat, target)
            }
        }
    }
}

fn tokens_appear_in_order(haystack: &str, terms: &[String]) -> bool {
    if terms.len() < 2 {
        return false;
    }
    let mut cursor = 0usize;
    for term in terms {
        if let Some(rel) = haystack[cursor..].find(term.as_str()) {
            cursor += rel + term.len();
        } else {
            return false;
        }
    }
    true
}

fn score_entry_everything(entry: &CachedFileMeta, parsed: &ParsedEverythingQuery) -> Option<f32> {
    // 1. Exclusiones estilo Everything (!termino / -termino)
    for ex in &parsed.excludes {
        if atom_matches_text(ex, &entry.path_folded) || atom_matches_text(ex, &entry.name_folded) {
            return None;
        }
    }

    // 2. Evaluar grupos AND (cada grupo tiene 1 o más alternativas OR)
    let mut all_non_path_in_name = true;
    let mut matched_any_in_name = false;
    let mut phrase_in_name_bonus = 0.0f32;
    let mut word_boundary_bonus = 0.0f32;
    let mut used_compact_fallback = false;

    for group in &parsed.and_groups {
        let mut group_in_name = false;
        let mut group_in_path = false;

        for atom in group {
            if !atom.path_only && atom_matches_text(atom, &entry.name_folded) {
                group_in_name = true;
                group_in_path = true;
                matched_any_in_name = true;
                if atom.kind == AtomKind::Phrase {
                    phrase_in_name_bonus += 320.0;
                } else if atom.kind == AtomKind::Token && is_word_boundary_match(&entry.name_folded, &atom.text) {
                    word_boundary_bonus += 100.0;
                }
                break;
            }
            if atom_matches_text(atom, &entry.path_folded) {
                group_in_path = true;
                if atom.kind == AtomKind::Token && is_word_boundary_match(&entry.path_folded, &atom.text) {
                    word_boundary_bonus += 25.0;
                }
            }
        }

        if !group_in_path {
            // Respaldo compacto alfanumérico (ej. "contrato2026" -> "contrato_2026.pdf") solo para consultas sin path_only
            if parsed.positive_compact.len() >= 4
                && parsed.and_groups.len() == 1
                && group.iter().all(|a| !a.path_only && a.kind == AtomKind::Token)
                && compact_alnum(&entry.name_folded).contains(&parsed.positive_compact)
            {
                group_in_name = true;
                group_in_path = true;
                matched_any_in_name = true;
                used_compact_fallback = true;
            }
        }

        if !group_in_path {
            return None;
        }
        if !group_in_name && group.iter().all(|a| !a.path_only) {
            all_non_path_in_name = false;
        }
    }

    // 3. Ranking de alta precisión estilo Everything (coincidencias exactas y en nombre siempre primero)
    let mut score = 0.0f32;
    let q = &parsed.positive_folded;

    if !q.is_empty() {
        if entry.name_folded == *q || entry.stem_folded == *q {
            score += 2000.0;
        } else if entry.name_folded.starts_with(q.as_str()) {
            score += 1200.0;
        } else if entry.name_folded.contains(q.as_str()) {
            score += 780.0;
        } else if used_compact_fallback {
            score += 420.0;
        }
    }

    if all_non_path_in_name && matched_any_in_name {
        score += 460.0;
        if tokens_appear_in_order(&entry.name_folded, &parsed.ordered_terms) {
            score += 220.0;
        }
    } else if matched_any_in_name {
        score += 220.0;
    } else {
        // Coincidencia solo por ruta/carpeta padre
        score += 65.0;
    }

    score += phrase_in_name_bonus + word_boundary_bonus;

    if !entry.is_directory {
        score += 40.0;
        score += match entry.category {
            FileCategory::Document => 60.0,
            FileCategory::ImageOrDesign => 55.0,
            FileCategory::Media => 50.0,
            FileCategory::SystemOrBinary => 0.0,
        };
    } else {
        score -= 25.0;
    }

    if entry.noisy_path {
        score -= 280.0;
    }

    score -= (entry.name_folded.len() as f32) * 0.25;
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ranked_name_search_filters_categories_and_penalizes_cache() {
        let tmp = std::env::temp_dir().join(format!("dragon_storage_test_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let storage = DragonStorage::open(&tmp).unwrap();

        let make_rec = |id: u64, name: &str, path: &str| FileRecord {
            id,
            parent_id: 0,
            name: name.to_string(),
            path: Some(PathBuf::from(path)),
            size: 1024,
            attributes: 0,
            modified_ms: 1000,
            is_directory: false,
        };

        storage
            .insert_batch(&[
                make_rec(1, "contrato.pdf", r"C:\Users\Drhapso\Documents\contrato.pdf"),
                make_rec(2, "contrato_copia_backup_2025.pdf", r"C:\Users\Drhapso\Documents\target\debug\contrato_copia_backup_2025.pdf"),
                make_rec(3, "logo_empresa.svg", r"C:\Users\Drhapso\Documents\logo_empresa.svg"),
                make_rec(4, "trailer_evento.mp4", r"C:\Users\Drhapso\Videos\trailer_evento.mp4"),
                make_rec(5, "kernel_driver.dll", r"C:\Custom\kernel_driver.dll"),
            ])
            .unwrap();

        let docs = storage.search_by_name_ranked("contrato", Some(FileCategory::Document), 10).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].0.name, "contrato.pdf");

        let design = storage.search_by_name_ranked("logo svg", Some(FileCategory::ImageOrDesign), 10).unwrap();
        assert_eq!(design.len(), 1);
        assert_eq!(design[0].0.name, "logo_empresa.svg");

        let media = storage.search_by_name_ranked("trailer", Some(FileCategory::Media), 10).unwrap();
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].0.name, "trailer_evento.mp4");

        let sys = storage.search_by_name_ranked("kernel", Some(FileCategory::SystemOrBinary), 10).unwrap();
        assert_eq!(sys.len(), 1);
        assert_eq!(sys[0].0.name, "kernel_driver.dll");

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn extension_index_tracks_inserts_updates_deletes_and_prefix_search() {
        let tmp = std::env::temp_dir().join(format!(
            "dragon_storage_ext_test_{}.redb",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let storage = DragonStorage::open(&tmp).unwrap();

        let make_rec = |id: u64, name: &str, path: &str| FileRecord {
            id,
            parent_id: 0,
            name: name.to_string(),
            path: Some(PathBuf::from(path)),
            size: 2048,
            attributes: 0,
            modified_ms: 2000,
            is_directory: false,
        };

        storage
            .insert_batch(&[
                make_rec(10, "presupuesto_2026.xlsx", r"C:\Users\Drhapso\Documents\presupuesto_2026.xlsx"),
                make_rec(11, "notas_reunion.xlsx", r"C:\Users\Drhapso\Documents\node_modules\notas_reunion.xlsx"),
                make_rec(12, "contrato_final.pdf", r"C:\Users\Drhapso\Documents\contrato_final.pdf"),
                make_rec(13, "plano_planta.dwg", r"C:\Users\Drhapso\CAD\plano_planta.dwg"),
            ])
            .unwrap();

        assert!(storage.extension_exists(".XLSX").unwrap());
        assert!(storage.extension_exists("pdf").unwrap());
        assert!(!storage.extension_exists("pptx").unwrap());

        let xlsx_hits = storage
            .search_by_extension(&["*.xlsx".to_string()], &[], None, false, 10)
            .unwrap();
        assert_eq!(xlsx_hits.len(), 2);
        assert_eq!(xlsx_hits[0].0.name, "presupuesto_2026.xlsx");

        let filtered = storage
            .search_by_extension(
                &["xlsx".to_string()],
                &["presupuesto".to_string()],
                None,
                false,
                10,
            )
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0.name, "presupuesto_2026.xlsx");

        let prefix_hits = storage
            .search_by_extension(&["xl".to_string()], &[], None, true, 10)
            .unwrap();
        assert_eq!(prefix_hits.len(), 2);

        let top = storage.list_top_extensions(None, 10).unwrap();
        assert_eq!(top[0].extension, "xlsx");
        assert_eq!(top[0].count, 2);

        // Renombrar ID 13 a .dxf debe actualizar el índice de extensiones
        storage
            .insert_batch(&[make_rec(13, "plano_planta.dxf", r"C:\Users\Drhapso\CAD\plano_planta.dxf")])
            .unwrap();
        assert!(!storage.extension_exists("dwg").unwrap());
        assert!(storage.extension_exists("dxf").unwrap());

        // Eliminar ID 12 debe purgar pdf del índice de extensiones
        storage
            .remove_by_paths(&[(
                r"c:\users\drhapso\documents\contrato_final.pdf".to_string(),
                r"C:\Users\Drhapso\Documents\contrato_final.pdf".to_string(),
                12,
            )])
            .unwrap();
        assert!(!storage.extension_exists("pdf").unwrap());

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn name_and_extension_search_are_accent_and_separator_insensitive() {
        let tmp = std::env::temp_dir().join(format!(
            "dragon_storage_accent_test_{}.redb",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let storage = DragonStorage::open(&tmp).unwrap();

        let make_rec = |id: u64, name: &str, path: &str| FileRecord {
            id,
            parent_id: 0,
            name: name.to_string(),
            path: Some(PathBuf::from(path)),
            size: 1024,
            attributes: 0,
            modified_ms: 1000,
            is_directory: false,
        };

        storage
            .insert_batch(&[
                make_rec(
                    1,
                    "Resolución No. 7870 Política.pdf",
                    r"D:\2026\Normograma\RESOLUCIONES\Resolución No. 7870 Política.pdf",
                ),
                make_rec(
                    2,
                    "informacion_tecnica-2026.docx",
                    r"D:\Telemática\informacion_tecnica-2026.docx",
                ),
                make_rec(
                    3,
                    "anexo_01.xlsx",
                    r"D:\2026\Contratación\anexo_01.xlsx",
                ),
            ])
            .unwrap();

        // Sin tilde debe encontrar "Resolución"
        let hits = storage.search_by_name_ranked("resolucion politica", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, 1);

        // Con tilde debe encontrar "informacion_tecnica-2026.docx" tolerando separadores
        let hits2 = storage.search_by_name_ranked("información técnica 2026", None, 10).unwrap();
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].0.id, 2);

        // Un solo token en carpeta padre ("contratacion" sin tilde) debe encontrar archivo dentro de "Contratación"
        let hits3 = storage.search_by_name_ranked("contratacion", None, 10).unwrap();
        assert_eq!(hits3.len(), 1);
        assert_eq!(hits3[0].0.id, 3);

        // Búsqueda por extensión + token sin tilde debe encontrar "Resolución"
        let ext_hits = storage
            .search_by_extension(&["pdf".to_string()], &["resolucion".to_string()], None, false, 10)
            .unwrap();
        assert_eq!(ext_hits.len(), 1);
        assert_eq!(ext_hits[0].0.id, 1);

        // Operadores estilo Everything: comodines *, frases "...", exclusiones !/-, OR | y prefijo de unidad
        let wild = storage.search_by_name_ranked("resolucion*7870*.pdf", None, 10).unwrap();
        assert_eq!(wild.len(), 1);
        assert_eq!(wild[0].0.id, 1);

        let phrase = storage.search_by_name_ranked("\"no. 7870\"", None, 10).unwrap();
        assert_eq!(phrase.len(), 1);
        assert_eq!(phrase[0].0.id, 1);

        let excl = storage.search_by_name_ranked("2026 !contratacion", None, 10).unwrap();
        assert_eq!(excl.len(), 2);
        assert!(excl.iter().all(|(r, _)| r.id != 3));

        let or_hits = storage.search_by_name_ranked("resolucion | anexo", None, 10).unwrap();
        assert_eq!(or_hits.len(), 2);

        let path_drive = storage.search_by_name_ranked(r"d:\telematica 2026", None, 10).unwrap();
        assert_eq!(path_drive.len(), 1);
        assert_eq!(path_drive[0].0.id, 2);

        let _ = std::fs::remove_file(&tmp);
    }
}
