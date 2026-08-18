mod command;
mod config;
mod error;
mod state;
mod static_files;

use std::net::SocketAddr;

use axum::Router;
use state::AppStateRef;
use tower_http::trace::TraceLayer;
use tracing::info;

/// Pebble Web 服务入口。
///
/// 阶段三骨架：加载配置、初始化共享状态、装配路由（健康检查 + 命令入口），
/// 验证可独立编译与启动。命令实现、鉴权、同步、WebSocket 在后续阶段接入。
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::from_env();
    info!(
        port = config.port,
        data_dir = %config.data_dir.display(),
        "pebble-web starting"
    );

    let state: AppStateRef = state::AppState::init(config);

    let port = state.config.port;

    let app = Router::new()
        .route("/api/v1/health", axum::routing::get(command::health))
        .route(
            "/api/v1/command/{command}",
            axum::routing::post(command::handle_command),
        )
        .fallback(command::not_found)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = format!("0.0.0.0:{port}")
        .parse()
        .expect("invalid listen address");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind listener");
    info!(%addr, "pebble-web listening");
    axum::serve(listener, app).await.expect("server error");
}