//! dragon-service: Ejecutable del servicio en segundo plano / daemon MFT.
//! Se ejecuta con privilegios de Administrador/SYSTEM para leer el MFT sin que
//! la interfaz gráfica o los usuarios requieran permisos elevados repetidamente.

use dragon_service::{DEFAULT_PIPE_NAME, IpcRequest, IpcResponse};
use std::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║             DRAGON RADAR TOOL - MFT SERVICE              ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!("Iniciando servicio de fondo con privilegios de disco...");
    println!("Tubo con nombre activo en: {}", DEFAULT_PIPE_NAME);

    // En Windows se abre el Named Pipe server para aceptar conexiones de clientes no elevados
    #[cfg(windows)]
    run_named_pipe_server().await?;

    Ok(())
}

#[cfg(windows)]
async fn run_named_pipe_server() -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    log::info!("Escuchando en el Named Pipe: {}", DEFAULT_PIPE_NAME);

    loop {
        let server = ServerOptions::new()
            .first_pipe_instance(false)
            .create(DEFAULT_PIPE_NAME)?;

        log::info!("Esperando conexión de cliente...");
        server.connect().await?;
        log::info!("Cliente conectado al servicio MFT");

        tokio::spawn(async move {
            let mut server = server;
            let mut buf = vec![0u8; 4096];

            loop {
                match server.read(&mut buf).await {
                    Ok(0) => break, // Cliente desconectado
                    Ok(n) => {
                        let request: Result<IpcRequest, _> = serde_json::from_slice(&buf[..n]);
                        let response = match request {
                            Ok(IpcRequest::Ping) => IpcResponse::Pong,
                            Ok(IpcRequest::ScanVolume { volume_letter }) => {
                                log::info!("Petición recibida para escanear MFT de unidad {}:", volume_letter);
                                let t0 = Instant::now();
                                match dragon_mft::ntfs::win::scan_volume_mft(volume_letter) {
                                    Ok(res) => IpcResponse::ScanComplete {
                                        total_files: res.metrics.total_files,
                                        total_directories: res.metrics.total_directories,
                                        duration_ms: t0.elapsed().as_millis() as u64,
                                    },
                                    Err(err) => IpcResponse::Error(err),
                                }
                            }
                            Ok(IpcRequest::GetMetrics) => IpcResponse::Error("No implementado".into()),
                            Err(e) => IpcResponse::Error(format!("Mensaje IPC no reconocido: {}", e)),
                        };

                        if let Ok(resp_bytes) = serde_json::to_vec(&response) {
                            let _ = server.write_all(&resp_bytes).await;
                        }
                    }
                    Err(e) => {
                        log::error!("Error leyendo del cliente IPC: {}", e);
                        break;
                    }
                }
            }
        });
    }
}
