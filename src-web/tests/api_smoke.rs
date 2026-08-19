//! Web API 冒烟测试：不绑端口，直接用 tower oneshot 驱动 Router。
//! 覆盖计划书 §52 要求的基础路径：未登录访问 / 正常请求 / 参数错误 / 数据不存在 / 错误转换。

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use pebble_web::{auth, build_app, config::Config};
use serde_json::json;
use tower::ServiceExt;

fn test_config() -> Config {
    let dir = std::env::temp_dir().join(format!("pw-api-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    Config {
        port: 0,
        data_dir: dir,
        password_hash: auth::hash_password("test-password").unwrap(),
        jwt_secret: "this-is-a-real-secret-with-32-plus-chars".to_string(),
        sync_interval_secs: 3600,
        static_dir: std::path::PathBuf::from("./dist"),
    }
}

async fn new_app() -> Router {
    let (app, _) = build_app(test_config()).await.unwrap();
    app
}

async fn request(
    router: &Router,
    method: &str,
    path: &str,
    body: &str,
    token: Option<&str>,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(path);
    if method == "POST" {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn login_token(router: &Router) -> String {
    let (status, body) = request(router, "POST", "/api/v1/command/login", r#"{"password":"test-password"}"#, None).await;
    assert_eq!(status, StatusCode::OK, "login failed: {body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn health_returns_ok() {
    let app = new_app().await;
    let (status, body) = request(&app, "GET", "/api/v1/health", "", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"status\":\"ok\""));
    assert!(body.contains("pebble-web"));
}

#[tokio::test]
async fn login_ok_and_rejects_wrong_password() {
    let app = new_app().await;
    let (status, body) = request(&app, "POST", "/api/v1/command/login", r#"{"password":"test-password"}"#, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("token"));

    let (status, body) = request(&app, "POST", "/api/v1/command/login", r#"{"password":"wrong"}"#, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("UNAUTHORIZED"));
}

#[tokio::test]
async fn protected_command_requires_token() {
    let app = new_app().await;
    let (status, body) = request(&app, "POST", "/api/v1/command/list_accounts", "{}", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("missing bearer token"));
}

#[tokio::test]
async fn unknown_command_returns_404() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let (status, body) = request(&app, "POST", "/api/v1/command/nope", "{}", Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("unknown command"));
}

#[tokio::test]
async fn invalid_json_rejected() {
    let app = new_app().await;
    let (status, body) = request(&app, "POST", "/api/v1/command/login", "not-json", None).await;
    eprintln!("invalid_json status={status} body={body:?}");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("error"));
}

#[tokio::test]
async fn add_account_validates_args() {
    let app = new_app().await;
    let token = login_token(&app).await;
    // 缺必填字段 → 400
    let (status, body) = request(&app, "POST", "/api/v1/command/add_account", "{}", Some(&token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(body.contains("BAD_REQUEST"));

    // 非法 provider → 400
    let bad = json!({
        "email": "a@b.com", "display_name": "A", "provider": "gmail",
        "imap_host": "x", "imap_port": 993, "smtp_host": "x", "smtp_port": 465,
        "username": "a", "password": "p", "imap_security": "tls", "smtp_security": "tls"
    });
    let (status, body) = request(&app, "POST", "/api/v1/command/add_account", &bad.to_string(), Some(&token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(body.contains("not supported yet"));
}

#[tokio::test]
async fn accounts_crud_roundtrip() {
    let app = new_app().await;
    let token = login_token(&app).await;

    let acc = json!({
        "email": "crud@example.com", "display_name": "Crud", "provider": "imap",
        "imap_host": "imap.example.com", "imap_port": 993, "smtp_host": "smtp.example.com",
        "smtp_port": 465, "username": "crud", "password": "pw",
        "imap_security": "tls", "smtp_security": "tls"
    });
    let (status, body) = request(&app, "POST", "/api/v1/command/add_account", &acc.to_string(), Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "add failed: {body}");
    let account: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = account["id"].as_str().unwrap();

    let (status, body) = request(&app, "POST", "/api/v1/command/list_accounts", "{}", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    // 删除不存在 → 幂等 200（与 store.delete_account 语义一致：无行删除不报错）
    let (status, _) = request(&app, "POST", "/api/v1/command/delete_account", "{\"account_id\":\"no-such-id\"}", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);

    let del = json!({ "account_id": id });
    let (status, body) = request(&app, "POST", "/api/v1/command/delete_account", &del.to_string(), Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "delete failed: {body}");
}