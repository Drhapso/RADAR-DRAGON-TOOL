//! dragon-core: Tipos de datos fundamentales, estructuras de metadatos
//! y configuración compartida de Dragon Radar Tool.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    /// Búsqueda instantánea de nombres (MFT RAM Cache / Redb).
    Filename,
    /// Búsqueda de contenido indexado (Tantivy).
    Content,
    /// Búsqueda semántica por significado (Vectores).
    Semantic,
    /// Fusión híbrida de resultados ponderados.
    Hybrid,
}

/// Categoría funcional de cada archivo para controlar qué se indexa por contenido vs solo por nombre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileCategory {
    /// Documentos (PDF, Office, OpenDocument, eBooks, texto, datos y código): nombre + contenido.
    Document,
    /// Audio y video en todos los formatos conocidos: nombre únicamente.
    Media,
    /// Imágenes, vectores, diseño gráfico, CAD y 3D: nombre únicamente.
    ImageOrDesign,
    /// Archivos de sistema, ejecutables, librerías, binarios y comprimidos: nombre únicamente.
    SystemOrBinary,
}

/// Clasifica un archivo según su extensión para aplicar reglas de indexación y prioridad.
pub fn classify_path<P: AsRef<Path>>(path: P) -> FileCategory {
    let ext = path
        .as_ref()
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    classify_extension(&ext)
}

pub fn classify_extension(ext: &str) -> FileCategory {
    match ext {
        // 1. Documentos (Ofimática, portables, texto, datos y código) -> Nombre + Contenido
        "pdf" | "doc" | "docx" | "docm" | "dotx" | "dotm" | "odt" | "ott" | "rtf"
        | "xls" | "xlsx" | "xlsm" | "xlsb" | "ods" | "csv" | "tsv"
        | "ppt" | "pptx" | "pptm" | "ppsx" | "odp"
        | "epub" | "mobi" | "azw3" | "fb2" | "djvu" | "one"
        | "txt" | "md" | "markdown" | "rst" | "adoc" | "tex" | "org" | "log"
        | "json" | "jsonl" | "ndjson" | "xml" | "yaml" | "yml" | "toml" | "ini" | "cfg"
        | "conf" | "env" | "properties" | "sql" | "graphql" | "proto"
        | "html" | "htm" | "xhtml" | "css" | "scss" | "sass" | "less"
        | "js" | "mjs" | "cjs" | "ts" | "mts" | "jsx" | "tsx" | "vue" | "svelte" | "astro"
        | "php" | "rs" | "py" | "pyw" | "ipynb" | "java" | "kt" | "kts" | "scala"
        | "go" | "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" | "hxx" | "cs" | "fs" | "vb"
        | "rb" | "pl" | "lua" | "r" | "dart" | "swift" | "m" | "mm"
        | "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "bat" | "cmd"
        | "gradle" | "cmake" => FileCategory::Document,

        // 2. Multimedia (Audio y Video en todos los formatos conocidos) -> Solo Nombre
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "mpg" | "mpeg"
        | "3gp" | "3g2" | "m2ts" | "vob" | "ogv" | "rm" | "rmvb" | "asf"
        | "divx" | "f4v" | "mp3" | "wav" | "flac" | "aac" | "ogg" | "oga" | "opus"
        | "wma" | "m4a" | "m4b" | "aiff" | "aif" | "alac" | "ape" | "wv" | "mid"
        | "midi" | "amr" | "ac3" | "dts" | "caf" | "srt" | "ass" | "ssa" | "vtt"
        | "m3u" | "m3u8" | "pls" => FileCategory::Media,

        // 3. Imágenes, Vectores, Diseño Gráfico, CAD y 3D -> Solo Nombre
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tiff" | "tif" | "ico" | "cur"
        | "heic" | "heif" | "avif" | "jxl" | "tga" | "pcx" | "dds" | "exr" | "hdr"
        | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "orf" | "rw2" | "raf" | "sr2" | "pef"
        | "svg" | "svgz" | "ai" | "eps" | "cdr" | "cmx" | "psd" | "psb" | "pdd" | "xcf"
        | "kra" | "clip" | "afdesign" | "afphoto" | "afpub" | "sketch" | "fig" | "xd"
        | "indd" | "indt" | "idml" | "qxd" | "pub" | "fh11"
        | "dwg" | "dxf" | "dwf" | "dgn" | "step" | "stp" | "iges" | "igs" | "stl"
        | "obj" | "fbx" | "glb" | "gltf" | "blend" | "blend1" | "3ds" | "max" | "ma"
        | "mb" | "c4d" | "zpr" | "ztl" | "skp" | "rvt" | "rfa" | "ifc"
        | "prproj" | "aep" | "drp" | "veg" | "kdenlive" | "camproj" => FileCategory::ImageOrDesign,

        // 4. Sistema, Binarios, Ejecutables, Comprimidos y Otros -> Solo Nombre
        _ => FileCategory::SystemOrBinary,
    }
}

/// Detecta rutas de compilación, caché o dependencias que suelen generar ruido en búsquedas.
pub fn is_noisy_build_or_cache_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(r"\target\debug\")
        || lower.contains(r"\target\release\")
        || lower.contains(r"\.fingerprint\")
        || lower.contains(r"\node_modules\")
        || lower.contains(r"\.git\")
        || lower.contains(r"\__pycache__\")
        || lower.contains(r"\.cargo\registry\")
        || lower.contains(r"\appdata\")
}
