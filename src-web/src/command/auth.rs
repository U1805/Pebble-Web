use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 命令接口专用类型（计划书 §20）：避免与核心 crate 类型同义重复定义。
#[derive(Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
}

/// 公开命令：登录，签发 7 天有效期的 JWT。
pub fn login(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let body: LoginRequest = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid login args: {e}")))?;
    if !auth::verify_password(&body.password, &state.config.password_hash) {
        return Err(ApiError::Unauthorized("invalid password".to_string()));
    }
    let token = auth::create_token(&state.config.jwt_secret, 7)
        .map_err(|e| ApiError::Internal(format!("token creation failed: {e}")))?;
    serde_json::to_value(LoginResponse { token })
        .map_err(|e| ApiError::Internal(format!("json encode failed: {e}")))
}
