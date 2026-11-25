//! dragon-storage: Motor de persistencia compacto basado en redb (B-Tree MVCC en Rust).
//! Evita los desbordamientos masivos de archivos WAL que afectan a SQLite en indexadores
//! masivos, garantizando ACID y auto-reclamación eficiente de espacio en disco.

use anyhow::{Context, Result};
use dragon_core::FileRecord;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// Definición de tablas tipadas
const FILES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("files_metadata");
const PATH_INDEX_TABLE: TableDefinition<&str, u64> = TableDefinition::new("path_to_id");
const SETTINGS_TABLE: TableDefinition<&str, &str> = TableDefinition::new("settings");
const INDEX_STATE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("index_state");
const CHECKPOINT_SETTING_KEY: &str = "indexing_checkpoint";

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

pub struct DragonStorage {
    db: Database,
}

impl DragonStorage {
    /// Abre o inicializa la base de datos embebida en un único archivo.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db_path = path.as_ref();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let db = Database::create(db_path)
            .context("Error abriendo la base de datos redb de Dragon Radar")?;

        // Inicializar tablas si no existen
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(FILES_TABLE)?;
            let _ = write_txn.open_table(PATH_INDEX_TABLE)?;
            let _ = write_txn.open_table(SETTINGS_TABLE)?;
            let _ = write_txn.open_table(INDEX_STATE_TABLE)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    /// Inserta o actualiza un lote de registros de archivos en una sola transacción atómica.
    pub fn insert_batch(&self, records: &[FileRecord]) -> Result<usize> {
        let write_txn = self.db.begin_write()?;
        let mut count = 0;

        {
            let mut files_table = write_txn.open_table(FILES_TABLE)?;
            let mut path_table = write_txn.open_table(PATH_INDEX_TABLE)?;

            for record in records {
                let serialized = serde_json::to_vec(record)?;
                files_table.insert(record.id, serialized.as_slice())?;

                if let Some(ref path) = record.path {
                    if let Some(path_str) = path.to_str() {
                        path_table.insert(path_str, record.id)?;
                    }
                }
                count += 1;
            }
        }

        write_txn.commit()?;
        Ok(count)
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

    /// Busca registros de archivos por coincidencia de subcadena en el nombre (case-insensitive).
    pub fn search_by_name(&self, query: &str, limit: usize) -> Result<Vec<FileRecord>> {
        let q_lower = query.to_lowercase();
        let read_txn = self.db.begin_read()?;
        let files_table = read_txn.open_table(FILES_TABLE)?;
        let mut results = Vec::with_capacity(limit);

        for item in files_table.iter()? {
            let (_, val) = item?;
            let record: FileRecord = serde_json::from_slice(val.value())?;
            if record.name.to_lowercase().contains(&q_lower) {
                results.push(record);
                if results.len() >= limit {
                    break;
                }
            }
        }
        Ok(results)
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
            let mut state_table = write_txn.open_table(INDEX_STATE_TABLE)?;

            for (path_key, original_path, doc_id) in items {
                let _ = state_table.remove(path_key.as_str())?;
                let _ = path_table.remove(original_path.as_str())?;
                let _ = files_table.remove(*doc_id)?;
                removed += 1;
            }
        }
        write_txn.commit()?;
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
