//! Pebble Web 服务库入口。
//!
//! main.rs 仅为薄壳；真正组装逻辑在此，便于集成测试直接构造 Router。

pub mod account_colors;
pub mod auth;
pub(crate) mod browser_notifications;
pub(crate) mod blocking;
pub mod commands;
pub mod command_router;
pub mod config;
pub mod crypto;
pub mod error;
pub mod events;
pub(crate) mod oauth;
#[path = "../patch/mod.rs"]
pub(crate) mod patch;
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

    match state.store.quick_check() {
        Ok(result) if result == "ok" => tracing::info!("Database integrity check passed"),
        Ok(result) => tracing::warn!("Database integrity check warning: {result}"),
        Err(error) => tracing::warn!("Database integrity check failed: {error}"),
    }

    // Snooze expiry is independent from search rebuild and may start
    // immediately, matching the desktop startup ordering.
    {
        let snooze_state = state.clone();
        tokio::spawn(async move {
            snooze_watcher::run_snooze_watcher(
                snooze_state.store.clone(),
                snooze_state.ws_broadcast.clone(),
            )
            .await;
        });
    }

    // Recover/rebuild search before starting sync and pending-op workers. A
    // concurrent sync could index a newly stored message just before
    // `do_reindex` clears the index and permanently lose that document.
    {
        let worker_state = state.clone();
        let search_needs_reindex = worker_state.search.needs_reindex();
        tokio::spawn(async move {
            let store = worker_state.store.clone();
            let search = worker_state.search.clone();
            let ws_broadcast = worker_state.ws_broadcast.clone();
            let reindex_result = tokio::task::spawn_blocking(move || {
                match commands::indexing::recover_pending_search_operations(&store, &search) {
                    Ok(recovered) if recovered > 0 => tracing::info!(
                        "Recovered {recovered} pending search operations from previous session"
                    ),
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        "Search recovery did not commit; pending operations were retained: {error}"
                    ),
                }

                let needs_rebuild = if search_needs_reindex {
                    tracing::info!("Search index schema changed, rebuild required");
                    true
                } else {
                    let index_count = search.doc_count();
                    let db_count = store.count_all_messages().unwrap_or(0);
                    if index_count == 0 && db_count > 0 {
                        tracing::info!(
                            "Search index empty but DB has {db_count} messages, rebuild required"
                        );
                        true
                    } else if index_count > 0 && index_count != db_count {
                        tracing::warn!(
                            "SQLite/Tantivy count mismatch (db={db_count}, index={index_count}), rebuilding"
                        );
                        true
                    } else {
                        false
                    }
                };

                if needs_rebuild {
                    match commands::indexing::do_reindex(&store, &search) {
                        Ok(count) => {
                            tracing::info!("Background reindex complete: {count} messages indexed");
                            if let Err(error) = store.clear_all_search_pending() {
                                tracing::warn!(
                                    "Search rebuild committed but recovery markers could not be cleared: {error}"
                                );
                            }
                            let _ = ws_broadcast.send(
                                serde_json::json!({
                                    "type": "search:reindex-complete",
                                    "payload": count,
                                })
                                .to_string(),
                            );
                        }
                        Err(error) => tracing::error!("Background reindex failed: {error}"),
                    }
                }
            })
            .await;

            if let Err(error) = reindex_result {
                tracing::error!("Background reindex task join failed: {error}");
            }

            worker_state.sync_manager.spawn();

            let pending_state = worker_state.clone();
            tokio::spawn(async move {
                commands::pending_mail_ops::run_pending_mail_ops_worker(pending_state).await;
            });

            tokio::spawn(async move {
                commands::cloud_sync::run_auto_backup_worker(worker_state).await;
            });
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
            post(commands::attachments::stage_compose_attachment_multipart)
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/v1/background-images/import",
            post(commands::appearance::import_background_image_multipart).layer(
                DefaultBodyLimit::max(commands::appearance::MAX_BACKGROUND_MULTIPART_BODY_BYTES),
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
            "/api/v1/oauth/callback-status",
            post(oauth::oauth_callback_status),
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
