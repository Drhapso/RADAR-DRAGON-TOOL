//! dragon-service: Protocolo IPC y cliente de comunicación con el servicio en segundo plano.
//! Permite que clientes no elevados consulten lecturas de MFT a través de un Named Pipe seguro.

use serde::{Deserialize, Serialize};

pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\dragon_radar_mft";

/// Mensajes de solicitud enviados desde clientes no elevados al servicio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IpcRequest {
    Ping,
    ScanVolume { volume_letter: char },
    GetMetrics,
}

/// Mensajes de respuesta devueltos por el servicio MFT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IpcResponse {
    Pong,
    ScanStarted { volume: char },
    ScanComplete {
        total_files: usize,
        total_directories: usize,
        duration_ms: u64,
    },
    Error(String),
}
