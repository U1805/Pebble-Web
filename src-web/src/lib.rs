//! Pebble Web 服务库入口。
//!
//! main.rs 仅为薄壳；真正组装逻辑在此，便于集成测试直接构造 Router。

pub mod account_colors;
pub mod auth;
pub(crate) mod blocking;
pub mod commands;
pub mod command_router;
pub mod config;
pub mod crypto;
pub mod error;
pub mod events;
pub(crate) mod oauth;
pub mod profile;
pub mod state;
pub mod snooze_watcher;
pub mod sync_runtime;
pub mod realtime;

use axum::{extract::DefaultBodyLimit, routing::post, Router};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

/// 依据配置组装完整应用（状态初始化 + 路由 + 后台同步循环）。
/// 返回 (Router, 监听端口)，端口由 main 用于 bind。
pub async fn build_app(config: config::Config) -> Result<(Router, u16), String> {
    let state = state::AppState::init(config).map_err(|e| format!("state init failed: {e}"))?;
    let port = state.config.port;
    // SPA 静态服务：static_dir 指向前端构建产物，未命中路径回退 index.html
    // （前端路由由 React Router 接管；API 命令均为 POST /api/v1/command/*，不受影响）
    let static_dir = state.config.static_dir.clone();

    // 后台定时同步循环（阶段 4.6：同步逻辑）
    state.sync_manager.spawn();

    // 自动备份定时 worker（WebDAV 云端同步，与桌面端 run_auto_backup_worker 对齐）
    {
        let worker_state = state.clone();
        tokio::spawn(async move {
            commands::cloud_sync::run_auto_backup_worker(worker_state).await;
        });
    }

    let app = Router::new()
        .route("/api/v1/health", axum::routing::get(command_router::health))
        .route("/api/v1/ws", axum::routing::get(realtime::ws_handler))
        .route(
            "/api/v1/attachments/{attachment_id}/download",
            axum::routing::get(commands::attachments::download_attachment),
        )
        .route(
            "/api/v1/attachments/stage",
            post(commands::attachments::stage_compose_attachment_multipart).layer(
                DefaultBodyLimit::max(commands::attachments::MAX_MULTIPART_BODY_BYTES),
            ),
        )
        .route(
            "/api/v1/background-images/{filename}",
            axum::routing::get(commands::appearance::get_background_image),
        )
        .route(
            "/api/v1/oauth/callback",
            axum::routing::get(oauth::oauth_callback),
        )
        .route(
            "/api/v1/command/{command}",
            axum::routing::post(command_router::handle_command),
        )
        .fallback_service(
            ServeDir::new(&static_dir)
                .not_found_service(ServeFile::new(static_dir.join("index.html"))),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    Ok((app, port))
}
