use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// 命令业务错误：code = 稳定机器码，message = 人读说明。
///
/// 业务层错误统一走 HTTP 400 + 结构化 body，与传输层错误分离，
/// 保证前端调用层可稳定解析（计划书 §52 错误结构稳定要求）。
#[derive(Debug)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
}

impl CommandError {
    /// 阶段四命令迁移启用。
    #[allow(dead_code)]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self { code: "INVALID_ARGUMENT", message: message.into() }
    }
}

/// 传输层错误：未认证 / 无权限 / 未知命令 / 服务器内部错误。
/// Unauthorized/Internal 由阶段四鉴权与命令错误映射启用。
#[derive(Debug)]
#[allow(dead_code)]
pub enum ApiError {
    Unauthorized(String),
    BadRequest(String),
    NotFound(String),
    Internal(String),
}

impl ApiError {
    /// 包装核心 crate 错误为统一的内部错误（不泄漏内部细节给远端）。阶段四命令迁移启用。
    #[allow(dead_code)]
    pub fn from_core(err: pebble_core::PebbleError) -> Self {
        tracing::error!(%err, "core error");
        ApiError::Internal("internal error".to_string())
    }
}

impl From<CommandError> for ApiError {
    fn from(e: CommandError) -> Self {
        ApiError::BadRequest(format!("{}: {}", e.code, e.message))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg),
            ApiError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", msg),
        };
        (status, Json(json!({ "error": { "code": code, "message": message } }))).into_response()
    }
}