//! dragon-core: Tipos de datos fundamentales, estructuras de metadatos
//! y configuración compartida de Dragon Radar Tool.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Entrada de metadatos de un archivo o carpeta indexado.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    /// Identificador único (ID de registro MFT en NTFS o hash en fallback).
    pub id: u64,
    /// ID del directorio padre.
    pub parent_id: u64,
    /// Nombre del archivo o directorio.
    pub name: String,
    /// Ruta absoluta reconstruida (opcional en caché de RAM para ahorrar memoria).
    pub path: Option<PathBuf>,
    /// Tamaño en bytes (0 para directorios).
    pub size: u64,
    /// Atributos del sistema de archivos (Bitflags de Windows).
    pub attributes: u32,
    /// Timestamp de última modificación (Unix epoch en milisegundos).
    pub modified_ms: u64,
    /// Indicador si es un directorio.
    pub is_directory: bool,
}

impl FileRecord {
    pub const ATTR_READONLY: u32 = 0x00000001;
    pub const ATTR_HIDDEN: u32 = 0x00000002;
    pub const ATTR_SYSTEM: u32 = 0x00000004;
    pub const ATTR_DIRECTORY: u32 = 0x00000010;
    pub const ATTR_ARCHIVE: u32 = 0x00000020;

    #[inline]
    pub fn is_hidden(&self) -> bool {
        (self.attributes & Self::ATTR_HIDDEN) != 0
    }

    #[inline]
    pub fn is_system(&self) -> bool {
        (self.attributes & Self::ATTR_SYSTEM) != 0
    }
}

/// Métricas de rendimiento de una operación de indexación.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ScanMetrics {
    pub volume_or_path: String,
    pub total_files: usize,
    pub total_directories: usize,
    pub duration_ms: u64,
    pub scan_method: ScanMethod,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanMethod {
    #[default]
    NtfsMftDirect,
    FileSystemWalk,
}

/// Modos de búsqueda soportados por el motor de Dragon Radar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchMode {
    /// Búsqueda instantánea de nombres (MFT RAM Cache).
    Filename,
    /// Búsqueda de contenido indexado (Tantivy).
    Content,
    /// Búsqueda semántica por significado (Vectores).
    Semantic,
    /// Fusión híbrida de resultados ponderados.
    Hybrid,
}
