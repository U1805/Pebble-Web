use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::state::{AppState, AppStateRef};

/// 命令注册表：命令名 → 处理函数。
///
/// 采用计划书 §17/§19 的命令风格：POST /api/v1/command/{command}，
/// 命令名与桌面端 Tauri command 保持一致，减少前端分支代码。
/// 白名单机制：不允许的命令一律 404，不做动态分发。
pub async fn handle_command(
    State(_state): State<AppStateRef>,
    Path(command): Path<String>,
    Json(_args): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    match command.as_str() {
        // 阶段四起按命令注册：accounts / folders / threads / messages / ...
        _ => Err(ApiError::NotFound(format!(
            "unknown command: {command}"
        ))),
    }
}

/// 健康检查（无需登录）。
pub async fn health(State(_state): State<AppStateRef>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "pebble-web",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// 未匹配路由的统一 404。
pub async fn not_found() -> ApiError {
    ApiError::NotFound("not found".to_string())
}

// 保持 AppState 被引用，避免未来移除时遗漏（当前 handle_command 用 _state）。
#[allow(dead_code)]
fn _keep_state_type(_: AppState) {}