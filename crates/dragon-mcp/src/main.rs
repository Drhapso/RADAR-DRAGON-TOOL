//! dragon-mcp: Servidor stdio JSON-RPC 2.0 para Model Context Protocol.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();

    dragon_mcp::run_stdio_server().await
}
