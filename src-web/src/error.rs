use axum::{
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use pebble_core::PebbleError;
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
        Self {
            code: "INVALID_ARGUMENT",
            message: message.into(),
        }
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
    /// 核心 crate 错误 → Web transport 错误。
    ///
    /// Tauri 直接序列化 `PebbleError`，共享前端读取其中的 `message` 字段。
    /// Web 可以使用不同 HTTP 状态码，但必须保留同一个原始 message。
    pub fn from_pebble(err: PebbleError) -> Self {
        match err {
            // Mail-provider authentication is a command/business failure, not a
            // Pebble Web session failure. HTTP 401 is reserved for the Web login
            // token so the frontend does not log the user out when an IMAP/OAuth
            // account needs re-authentication.
            PebbleError::Validation(msg)
            | PebbleError::Auth(msg)
            | PebbleError::OAuth(msg)
            | PebbleError::UnsupportedProvider(msg) => ApiError::BadRequest(msg),
            PebbleError::Network(msg)
            | PebbleError::Storage(msg)
            | PebbleError::Sync(msg)
            | PebbleError::SyncCursorExpired(msg)
            | PebbleError::Rule(msg)
            | PebbleError::Translate(msg)
            | PebbleError::Privacy(msg)
            | PebbleError::Internal(msg)
            | PebbleError::TokenExpired(msg)
            | PebbleError::TokenRefreshFailed(msg) => {
                tracing::error!(message = %msg, "core error");
                ApiError::Internal(msg)
            }
        }
    }

    /// 存储层错误便捷映射。
    pub fn from_store(err: PebbleError) -> Self {
        Self::from_pebble(err)
    }

    /// JSON 序列化错误。
    pub fn from_serialize(err: serde_json::Error) -> Self {
        ApiError::Internal(format!("serialization failed: {err}"))
    }

    /// 旧版本接口（对外保持兼容），等价于 from_pebble 内部错误路径。
    #[allow(dead_code)]
    pub fn from_core(err: PebbleError) -> Self {
        Self::from_pebble(err)
    }
}

impl From<CommandError> for ApiError {
    fn from(e: CommandError) -> Self {
        ApiError::BadRequest(format!("{}: {}", e.code, e.message))
    }
}

/// 核心 crate 错误的一站式转换（命令层 `?` 直接可用）。
impl From<PebbleError> for ApiError {
    fn from(err: PebbleError) -> Self {
        ApiError::from_pebble(err)
    }
}

/// 命令模块 String 错误的统一收口（如 credentials 封装返回 String）。
impl From<String> for ApiError {
    fn from(msg: String) -> Self {
        ApiError::Internal(msg)
    }
}

/// 请求体不是合法 JSON/参数错误时统一走结构化 400，替代 axum 默认文本响应。
impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        ApiError::BadRequest(e.body_text())
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
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}
