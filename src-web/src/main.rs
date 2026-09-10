use std::net::SocketAddr;

use tracing::info;
use tracing_subscriber::prelude::*; // SubscriberExt（registry.with）

/// Pebble Web 二进制入口：加载配置 → 初始化日志（stdout + 日志文件）→ build_app → 监听。
#[tokio::main]
async fn main() {
    let config = pebble_web::config::Config::from_env().expect("invalid configuration");

    // 日志双写：stdout（容器 docker logs）+ data_dir/logs/pebble-web.log（read_app_log 读取）
    let log_dir = config.data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).expect("failed to create log dir");
    let file_appender = tracing_appender::rolling::never(&log_dir, "pebble-web.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer()) // stderr
        .with(tracing_subscriber::fmt::layer().with_writer(non_blocking)) // 日志文件
        .init();
    // guard 在 main 期间保持存活，保证日志异步写入正常刷盘
    std::mem::forget(guard);

    info!(
        port = config.port,
        data_dir = %config.data_dir.display(),
        log_file = %config.log_path().display(),
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
