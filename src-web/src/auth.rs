use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::http::{header, HeaderMap};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ApiError;
use crate::state::AppStateRef;

/// JWT 载荷。单用户部署，sub 固定为 admin。
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
    pub iat: usize,
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

pub fn create_token(secret: &str, expiry_days: u64) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs() as usize;

    let claims = Claims {
        sub: "admin".to_string(),
        iat: now,
        exp: now + (expiry_days * 24 * 60 * 60) as usize,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| e.to_string())
}

pub fn validate_token(token: &str, secret: &str) -> Result<Claims, String> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map(|data| data.claims)
    .map_err(|e| e.to_string())
}

/// handler 内鉴权：命令路由统一走 POST /api/v1/command/{name}，
/// login 为公开命令，其余命令在此校验 Bearer token。
pub fn require_auth(state: &AppStateRef, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let Some(token) = token else {
        return Err(ApiError::Unauthorized("missing bearer token".to_string()));
    };
    validate_token(token, &state.config.jwt_secret)
        .map(|_| ())
        .map_err(|e| ApiError::Unauthorized(format!("invalid token: {e}")))
}

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
    if !verify_password(&body.password, &state.config.password_hash) {
        return Err(ApiError::Unauthorized("invalid password".to_string()));
    }
    let token = create_token(&state.config.jwt_secret, 7)
        .map_err(|e| ApiError::Internal(format!("token creation failed: {e}")))?;
    serde_json::to_value(LoginResponse { token })
        .map_err(|e| ApiError::Internal(format!("json encode failed: {e}")))
}
