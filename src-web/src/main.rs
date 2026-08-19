use std::net::SocketAddr;

use tracing::info;

/// Pebble Web 二进制入口：加载配置 → build_app → 监听。
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = pebble_web::config::Config::from_env().expect("invalid configuration");
    info!(
        port = config.port,
        data_dir = %config.data_dir.display(),
        "pebble-web starting"
    );

    let (app, port) = pebble_web::build_app(config)
        .await
        .expect("failed to build application");
    let addr: SocketAddr = format!("0.0.0.0:{port}")
        .parse()
        .expect("invalid listen address");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind listener");
    info!(%addr, "pebble-web listening");
    axum::serve(listener, app).await.expect("server error");
}