//! Pebble Web 服务库入口。
//!
//! main.rs 仅为薄壳；真正组装逻辑在此，便于集成测试直接构造 Router。

pub mod auth;
pub mod command;
pub mod config;
pub mod credentials;
pub mod crypto;
pub mod error;
pub mod state;
pub mod static_files;
pub mod sync;
pub mod ws;

use axum::Router;
use tower_http::trace::TraceLayer;

/// 依据配置组装完整应用（状态初始化 + 路由 + 后台同步循环）。
/// 返回 (Router, 监听端口)，端口由 main 用于 bind。
pub async fn build_app(config: config::Config) -> Result<(Router, u16), String> {
    let state = state::AppState::init(config).map_err(|e| format!("state init failed: {e}"))?;
    let port = state.config.port;

    // 后台定时同步循环（阶段 4.6：同步逻辑）
    state.sync_manager.spawn();

    let app = Router::new()
        .route("/api/v1/health", axum::routing::get(command::health))
        .route("/api/v1/ws", axum::routing::get(ws::ws_handler))
        .route(
            "/api/v1/attachments/{attachment_id}/download",
            axum::routing::get(command::attachments::download_attachment),
        )
        .route(
            "/api/v1/command/{command}",
            axum::routing::post(command::handle_command),
        )
        .fallback(command::not_found)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    Ok((app, port))
}