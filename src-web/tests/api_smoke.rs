//! Web API 冒烟测试：不绑端口，直接用 tower oneshot 驱动 Router。
//! 覆盖计划书 §52 要求的基础路径：未登录访问 / 正常请求 / 参数错误 / 数据不存在 / 错误转换。

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use pebble_core::now_timestamp;
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
    let (status, bytes) = request_bytes(router, method, path, body.as_bytes(), token).await;
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn request_bytes(
    router: &Router,
    method: &str,
    path: &str,
    body: &[u8],
    token: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(path);
    if method == "POST" {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = builder.body(Body::from(body.to_vec())).unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

async fn request_multipart_file(
    router: &Router,
    path: &str,
    filename: &str,
    bytes: &[u8],
    token: Option<&str>,
) -> (StatusCode, String) {
    let boundary = format!("----pebble-test-{}", uuid::Uuid::new_v4());
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        );
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = builder.body(Body::from(body)).unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn login_token(router: &Router) -> String {
    let (status, body) = request(
        router,
        "POST",
        "/api/v1/command/login",
        r#"{"password":"test-password"}"#,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login failed: {body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create_local_draft(router: &Router, token: &str, suffix: &str) -> String {
    let account_request = json!({
        "request": {
            "email": format!("{suffix}@example.com"),
            "display_name": "Test",
            "provider": "imap",
            "imap_host": "imap.example.com",
            "imap_port": 993,
            "smtp_host": "smtp.example.com",
            "smtp_port": 465,
            "username": suffix,
            "password": "pw",
            "imap_security": "tls",
            "smtp_security": "tls"
        }
    });
    let (status, body) = request(
        router,
        "POST",
        "/api/v1/command/add_account",
        &account_request.to_string(),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "test account failed: {body}");
    let account_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let draft = json!({
        "account_id": account_id,
        "to": ["recipient@example.com"],
        "subject": format!("{suffix} draft"),
        "body_text": "test body"
    });
    let (status, body) = request(
        router,
        "POST",
        "/api/v1/command/save_draft",
        &draft.to_string(),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "test draft failed: {body}");
    serde_json::from_str::<String>(&body).unwrap()
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
async fn health_check_command_is_public() {
    let app = new_app().await;
    let (status, body) = request(&app, "POST", "/api/v1/command/health_check", "{}", None).await;
    assert_eq!(status, StatusCode::OK, "health check failed: {body}");
    assert!(body.contains("Pebble is healthy"));
}

#[tokio::test]
async fn oauth_callback_rejects_unknown_or_replayed_state() {
    let app = new_app().await;
    let (status, body) = request(
        &app,
        "GET",
        "/api/v1/oauth/callback?state=unknown&code=example",
        "",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("invalid, expired, or already used"));
}

#[tokio::test]
async fn oauth_start_rejects_non_oauth_provider_before_configuration_lookup() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/complete_oauth_flow",
        r#"{"provider":"imap","email":"a@example.com","display_name":"A"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("unsupported OAuth provider"));
}

#[tokio::test]
async fn oauth_start_returns_provider_url_and_server_side_state() {
    std::env::set_var("GOOGLE_CLIENT_ID", "web-test-client");
    std::env::set_var(
        "PEBBLE_OAUTH_REDIRECT_URL",
        "https://mail.example.test/api/v1/oauth/callback",
    );
    let app = new_app().await;
    let token = login_token(&app).await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/complete_oauth_flow",
        r#"{"provider":"gmail","email":"fallback@example.com","display_name":"Fallback"}"#,
        Some(&token),
    )
    .await;
    std::env::remove_var("GOOGLE_CLIENT_ID");
    std::env::remove_var("PEBBLE_OAUTH_REDIRECT_URL");
    assert_eq!(status, StatusCode::OK, "OAuth start failed: {body}");
    assert!(body.contains("authorization_url"));
    assert!(body.contains("accounts.google.com"));
    assert!(body.contains("oauth%2Fcallback") || body.contains("oauth/callback"));
}

#[tokio::test]
async fn background_image_import_serve_and_delete_roundtrip() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3];
    let import = json!({
        "filename": "wallpaper.png",
        "bytes": bytes.clone()
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/import_background_image",
        &import.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "background import failed: {body}");
    let imported = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(
        imported["filename"].as_str().unwrap().ends_with(".png"),
        true
    );
    let path = imported["path"].as_str().unwrap().to_string();
    assert!(path.starts_with("/api/v1/background-images/background-"));

    let (status, returned) = request_bytes(&app, "GET", &path, b"", None).await;
    assert_eq!(status, StatusCode::OK, "background read failed");
    assert_eq!(returned, bytes);

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/delete_background_image",
        &json!({ "path": path }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "background delete failed: {body}");
}

#[tokio::test]
async fn realtime_preference_updates_web_scheduler_and_rejects_unknown_modes() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/set_realtime_preference",
        r#"{"mode":"battery"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "realtime preference failed: {body}");
    assert_eq!(body, "null");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/set_realtime_preference",
        r#"{"mode":"turbo"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "invalid realtime mode accepted: {body}"
    );
    assert!(body.contains("invalid realtime preference"));
}

#[tokio::test]
async fn global_proxy_commands_roundtrip_and_validate_pairs() {
    let app = new_app().await;
    let token = login_token(&app).await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_global_proxy",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "global proxy read failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!(null)
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/update_global_proxy",
        r#"{"proxy_host":" 127.0.0.1 ","proxy_port":7890}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "global proxy update failed: {body}");
    assert_eq!(body, "null");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_global_proxy",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!({"host":"127.0.0.1","port":7890})
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/update_global_proxy",
        r#"{"proxy_host":"127.0.0.1"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "partial proxy accepted: {body}"
    );
    assert!(body.contains("requires both host and port"));

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/update_global_proxy",
        r#"{"proxy_host":null,"proxy_port":null}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "global proxy clear failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_global_proxy",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!(null)
    );
}

#[tokio::test]
async fn rules_commands_crud_roundtrip() {
    let app = new_app().await;
    let token = login_token(&app).await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_rules",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rule list failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!([])
    );

    let create = json!({
        "name": "Mark important",
        "priority": 5,
        "conditions": r#"{"from":"alerts@example.com"}"#,
        "actions": r#"["star"]"#
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/create_rule",
        &create.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rule create failed: {body}");
    let mut rule: serde_json::Value = serde_json::from_str(&body).unwrap();
    let rule_id = rule["id"].as_str().unwrap().to_string();
    assert_eq!(rule["is_enabled"], true);
    assert_eq!(rule["priority"], 5);

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_rules",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], rule_id);

    rule["name"] = json!("Mark important (updated)");
    rule["is_enabled"] = json!(false);
    rule["updated_at"] = json!(rule["updated_at"].as_i64().unwrap() + 1);
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/update_rule",
        &json!({"rule": rule}).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rule update failed: {body}");
    assert_eq!(body, "null");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/delete_rule",
        &serde_json::to_string(&rule_id).unwrap(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rule delete failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_rules",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!([])
    );
}

#[tokio::test]
async fn read_app_log_returns_empty_snapshot_when_file_is_missing() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/read_app_log",
        r#"{"max_bytes":4096}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "diagnostic log read failed: {body}");
    let snapshot: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(snapshot["content"], "");
    assert_eq!(snapshot["truncated"], false);
    assert!(snapshot["path"]
        .as_str()
        .unwrap()
        .ends_with("logs/pebble-web.log"));

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/read_app_log",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "invalid log args accepted: {body}"
    );
}

#[tokio::test]
async fn labels_and_trusted_senders_roundtrip() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let message_id = create_local_draft(&app, &token, "labels").await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_accounts",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "account list failed: {body}");
    let account_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let sender = "alerts@example.com";

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_labels",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "label list failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!([])
    );

    let label_args = json!({"message_id": message_id, "label_name": "Important"});
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/add_message_label",
        &label_args.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "label add failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_message_labels",
        &json!({"message_id": message_id}).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "message labels read failed: {body}");
    let labels: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(labels.as_array().unwrap().len(), 1);
    assert_eq!(labels[0]["name"], "Important");
    assert_eq!(labels[0]["is_system"], false);

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_message_labels_batch",
        &json!({"message_ids": [message_id]}).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "batch labels read failed: {body}");
    let batch: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(batch[&message_id][0]["name"], "Important");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_labels",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/remove_message_label",
        &label_args.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "label remove failed: {body}");
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_message_labels",
        &json!({"message_id": message_id}).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!([])
    );

    let trust_args = json!({
        "account_id": account_id,
        "email": sender,
        "trust_type": "all"
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_trusted_senders",
        &json!({"account_id": account_id}).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "trusted sender list failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!([])
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/trust_sender",
        &trust_args.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "trust sender failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/is_trusted_sender",
        &json!({"account_id": account_id, "email": sender}).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!(true)
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/remove_trusted_sender",
        &json!({"account_id": account_id, "email": sender}).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "trusted sender remove failed: {body}"
    );
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/is_trusted_sender",
        &json!({"account_id": account_id, "email": sender}).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!(false)
    );
}

#[tokio::test]
async fn connection_tests_reject_plaintext_without_explicit_opt_in() {
    let app = new_app().await;
    let token = login_token(&app).await;

    let imap = json!({
        "request": {
            "imap_host": "imap.example.com",
            "imap_port": 143,
            "imap_security": "plain",
            "allow_plaintext": false,
            "username": "user",
            "password": "pw"
        }
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/test_imap_connection",
        &imap.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "plain IMAP accepted: {body}"
    );
    assert!(body.contains("plaintext connections are disabled"));

    let pop3 = json!({
        "request": {
            "pop3_host": "pop3.example.com",
            "pop3_port": 110,
            "pop3_security": "plain",
            "allow_plaintext": false,
            "username": "user",
            "password": "pw"
        }
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/test_pop3_connection",
        &pop3.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "plain POP3 accepted: {body}"
    );
    assert!(body.contains("plaintext connections are disabled"));
}

#[tokio::test]
async fn backup_export_preview_restore_and_auto_config_roundtrip() {
    let source = new_app().await;
    let source_token = login_token(&source).await;
    let account_request = json!({
        "request": {
            "email": "backup@example.com",
            "display_name": "Backup",
            "provider": "imap",
            "imap_host": "imap.example.com",
            "imap_port": 993,
            "smtp_host": "smtp.example.com",
            "smtp_port": 465,
            "username": "backup",
            "password": "secret-password",
            "imap_security": "tls",
            "smtp_security": "tls"
        }
    });
    let (status, body) = request(
        &source,
        "POST",
        "/api/v1/command/add_account",
        &account_request.to_string(),
        Some(&source_token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "backup account creation failed: {body}"
    );

    let (status, body) = request(
        &source,
        "POST",
        "/api/v1/command/export_backup_file",
        r#"{"secret_passphrase":"backup-pass"}"#,
        Some(&source_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "backup export failed: {body}");
    let backup_data: String = serde_json::from_str(&body).unwrap();
    let backup_json: serde_json::Value = serde_json::from_str(&backup_data).unwrap();
    assert_eq!(backup_json["accounts"].as_array().unwrap().len(), 1);
    assert!(backup_json["encrypted_secrets"].is_object());

    let (status, body) = request(
        &source,
        "POST",
        "/api/v1/command/preview_backup_file",
        &json!({"data": backup_data}).to_string(),
        Some(&source_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "backup preview failed: {body}");
    let preview: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(preview["account_count"], 1);
    assert_eq!(preview["has_encrypted_secrets"], true);
    assert_eq!(preview["secret_account_count"], 1);

    let restored = new_app().await;
    let restored_token = login_token(&restored).await;
    let (status, body) = request(
        &restored,
        "POST",
        "/api/v1/command/import_backup_file",
        &json!({"data": backup_data, "secret_passphrase":"backup-pass"}).to_string(),
        Some(&restored_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "backup restore failed: {body}");
    assert!(body.contains("restored with account passwords"));

    let (status, body) = request(
        &restored,
        "POST",
        "/api/v1/command/list_accounts",
        "{}",
        Some(&restored_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let accounts: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(accounts.as_array().unwrap().len(), 1);
    assert_eq!(accounts[0]["email"], "backup@example.com");

    let auto_config = json!({
        "config": {
            "url": "https://dav.example.test/pebble",
            "username": "dav-user",
            "password": "dav-password",
            "secret_passphrase": "backup-pass",
            "interval_minutes": 60,
            "enabled": true
        }
    });
    let (status, body) = request(
        &restored,
        "POST",
        "/api/v1/command/save_auto_backup_config",
        &auto_config.to_string(),
        Some(&restored_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "auto backup save failed: {body}");

    let (status, body) = request(
        &restored,
        "POST",
        "/api/v1/command/load_auto_backup_config",
        "{}",
        Some(&restored_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "auto backup load failed: {body}");
    let loaded: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(loaded["url"], "https://dav.example.test/pebble");
    assert_eq!(loaded["interval_minutes"], 60);
    assert_eq!(loaded["enabled"], true);

    let (status, body) = request(
        &restored,
        "POST",
        "/api/v1/command/save_auto_backup_config",
        &json!({"config": {"url":"https://dav.example.test","username":"u","password":"p","secret_passphrase":null,"interval_minutes":31,"enabled":true}}).to_string(),
        Some(&restored_token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unsupported backup interval accepted: {body}"
    );

    let (status, body) = request(
        &restored,
        "POST",
        "/api/v1/command/delete_auto_backup_config",
        "{}",
        Some(&restored_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "auto backup delete failed: {body}");
    let (status, body) = request(
        &restored,
        "POST",
        "/api/v1/command/load_auto_backup_config",
        "{}",
        Some(&restored_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!(null)
    );
}

#[tokio::test]
async fn pop3_connection_requires_credentials_before_network_access() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let request_body = json!({
        "request": {
            "pop3_host": "pop3.example.com",
            "pop3_port": 995,
            "pop3_security": "tls"
        }
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/test_pop3_connection",
        &request_body.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "missing POP3 credentials accepted: {body}"
    );
    assert!(body.contains("username and password are required"));
}

#[tokio::test]
async fn snooze_list_command_roundtrips_empty_state() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_snoozed",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "snooze list failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!([])
    );
}

#[tokio::test]
async fn snooze_commands_roundtrip_and_remove_record() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let message_id = create_local_draft(&app, &token, "snooze").await;
    let request_body = json!({
        "message_id": message_id.clone(),
        "until": now_timestamp() + 3600,
        "return_to": "archive"
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/snooze_message",
        &request_body.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "snooze failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_snoozed",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "snooze list failed: {body}");
    let listed = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["message_id"], message_id);
    assert_eq!(listed[0]["return_to"], "archive");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/unsnooze_message",
        &json!({ "message_id": message_id.clone() }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unsnooze failed: {body}");
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_snoozed",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!([])
    );
}

#[tokio::test]
async fn kanban_commands_roundtrip_empty_state() {
    let app = new_app().await;
    let token = login_token(&app).await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_kanban_cards",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "kanban cards failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!([])
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_kanban_context_notes",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "kanban notes failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!({})
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/set_kanban_context_note",
        r#"{"message_id":"kanban-1","note":"keep this thread"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "kanban note set failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["kanban-1"],
        "keep this thread"
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/merge_kanban_context_notes",
        r#"{"notes":{"kanban-1":"do not overwrite","kanban-2":"second note"}}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "kanban note merge failed: {body}");
    let notes = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(notes["kanban-1"], "keep this thread");
    assert_eq!(notes["kanban-2"], "second note");
}

#[tokio::test]
async fn kanban_card_commands_move_filter_and_remove() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let first_message_id = create_local_draft(&app, &token, "kanban-one").await;
    let second_message_id = create_local_draft(&app, &token, "kanban-two").await;
    let first_move = json!({
        "message_id": first_message_id.clone(),
        "column": "todo",
        "position": 3
    });

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/move_to_kanban",
        &first_move.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "kanban move failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/move_to_kanban",
        &json!({ "message_id": second_message_id.clone(), "column": "done" }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "second kanban move failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_kanban_cards",
        r#"{"column":"todo"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "kanban filter failed: {body}");
    let todo = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(todo.as_array().unwrap().len(), 1);
    assert_eq!(todo[0]["message_id"], first_message_id);
    assert_eq!(todo[0]["position"], 3);

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/remove_from_kanban",
        &json!({ "message_id": first_message_id.clone() }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "kanban remove failed: {body}");
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_kanban_cards",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let remaining = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(remaining.as_array().unwrap().len(), 1);
    assert_eq!(remaining[0]["message_id"], second_message_id);
}

#[tokio::test]
async fn translate_config_commands_roundtrip_encrypted_state() {
    let app = new_app().await;
    let token = login_token(&app).await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_translate_config",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "translate config read failed: {body}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!(null)
    );

    let config = json!({ "type": "deeplx", "endpoint": "http://localhost:1188/translate" });
    let save = json!({
        "provider_type": "deeplx",
        "config": config.to_string(),
        "is_enabled": true,
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/save_translate_config",
        &save.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "translate config save failed: {body}"
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_translate_config",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "translate config reread failed: {body}"
    );
    let loaded = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(loaded["provider_type"], "deeplx");
    assert_eq!(loaded["config"], config.to_string());
    assert_eq!(loaded["is_enabled"], true);

    let invalid = json!({
        "provider_type": "deeplx",
        "config": json!({ "type": "deeplx", "endpoint": "http://translate.example.test" }).to_string(),
        "is_enabled": true,
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/save_translate_config",
        &invalid.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "invalid translate config accepted: {body}"
    );
    assert!(body.contains("HTTPS"));
}

#[tokio::test]
async fn email_templates_and_signatures_roundtrip_secure_user_data() {
    let app = new_app().await;
    let token = login_token(&app).await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_email_templates",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "template list failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!([])
    );

    let template_args = json!({
        "template": {
            "name": "  Welcome  ",
            "subject": "Hello",
            "body": "Welcome to Pebble",
            "deduplicateByContent": true
        }
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/save_email_template",
        &template_args.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "template save failed: {body}");
    let saved = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    let template_id = saved["id"].as_str().unwrap().to_string();
    assert_eq!(saved["name"], "Welcome");
    assert!(saved["createdAt"].is_number());

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/save_email_template",
        &template_args.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "template deduplication failed: {body}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"],
        template_id
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/delete_email_template",
        &json!({ "id": template_id }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "template delete failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_email_signature",
        r#"{"accountId":"account-signature-1"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signature read failed: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        ""
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/set_email_signature",
        r#"{"accountId":"account-signature-1","signature":"Kind regards"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signature save failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/migrate_email_signature_if_absent",
        r#"{"accountId":"account-signature-1","signature":"legacy"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "signature migration read failed: {body}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        "Kind regards"
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/migrate_email_signature_if_absent",
        r#"{"accountId":"account-signature-2","signature":"legacy"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "legacy signature migration failed: {body}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        "legacy"
    );

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/set_email_signature",
        r#"{"accountId":"account-signature-1","signature":""}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signature clear failed: {body}");
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/migrate_email_signature_if_absent",
        r#"{"accountId":"account-signature-1","signature":"must not restore"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "cleared signature migration failed: {body}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        ""
    );
}

#[tokio::test]
async fn compose_attachment_staging_roundtrip_is_scoped_and_cleaned() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let (status, body) = request_multipart_file(
        &app,
        "/api/v1/attachments/stage",
        "unauthorized.txt",
        b"secret",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "unauthorized upload: {body}");

    let (status, body) = request_multipart_file(
        &app,
        "/api/v1/attachments/stage",
        "../report?.txt",
        b"hello",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "attachment stage failed: {body}");
    let path = serde_json::from_str::<String>(&body).unwrap();
    assert!(path.contains("compose_staging"));
    assert_eq!(std::fs::read(&path).unwrap(), b"hello");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/cleanup_staged_compose_attachment",
        &json!({ "path": path }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "attachment cleanup failed: {body}");
}

#[tokio::test]
async fn large_attachment_download_roundtrip_preserves_bytes() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let draft_id = create_local_draft(&app, &token, "large-download").await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_accounts",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let account_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Browser uploads use multipart, so the binary payload is not expanded
    // into a JSON number array and can exercise the real per-file limit.
    const LARGE_ATTACHMENT_SIZE: usize = 128 * 1024;
    let payload = vec![0x5a_u8; LARGE_ATTACHMENT_SIZE];
    let (status, body) = request_multipart_file(
        &app,
        "/api/v1/attachments/stage",
        "large.bin",
        &payload,
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "large attachment stage failed: {body}"
    );
    let staged_path = serde_json::from_str::<String>(&body).unwrap();

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/save_draft",
        &json!({
            "account_id": account_id,
            "to": ["recipient@example.com"],
            "subject": "large download",
            "body_text": "download test",
            "attachment_paths": [staged_path],
            "existing_draft_id": draft_id,
        })
        .to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "large attachment draft failed: {body}"
    );
    let saved_draft_id = serde_json::from_str::<String>(&body).unwrap();

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_attachments",
        &json!({ "message_id": saved_draft_id }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let attachments = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    let attachment_id = attachments[0]["id"].as_str().unwrap();

    let download_path = format!("/api/v1/attachments/{attachment_id}/download");
    let (status, downloaded) = request_bytes(&app, "GET", &download_path, &[], Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(downloaded.len(), LARGE_ATTACHMENT_SIZE);
    assert_eq!(downloaded, vec![0x5a_u8; LARGE_ATTACHMENT_SIZE]);

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/delete_draft",
        &json!({ "account_id": account_id, "draft_id": saved_draft_id }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "large attachment cleanup failed: {body}"
    );
}

#[tokio::test]
async fn webdav_connection_rejects_insecure_url_before_network_access() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/test_webdav_connection",
        r#"{"url":"http://webdav.example.com","username":"user","password":"secret"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.contains("HTTPS"),
        "unexpected WebDAV validation error: {body}"
    );
}

#[tokio::test]
async fn draft_commands_persist_and_delete_local_draft_with_attachment() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let account_request = json!({
        "request": {
            "email": "draft@example.com",
            "display_name": "Draft",
            "provider": "imap",
            "imap_host": "imap.example.com",
            "imap_port": 993,
            "smtp_host": "smtp.example.com",
            "smtp_port": 465,
            "username": "draft",
            "password": "pw",
            "imap_security": "tls",
            "smtp_security": "tls"
        }
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/add_account",
        &account_request.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "draft account failed: {body}");
    let account_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let staged = json!({ "filename": "draft.txt", "bytes": [100, 114, 97, 102, 116] });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/stage_compose_attachment",
        &staged.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "draft attachment stage failed: {body}"
    );
    let staged_path = serde_json::from_str::<String>(&body).unwrap();

    let draft_request = json!({
        "account_id": account_id,
        "to": ["recipient@example.com"],
        "cc": [],
        "bcc": [],
        "subject": "Draft subject",
        "body_text": "Draft body",
        "body_html": null,
        "in_reply_to": null,
        "attachment_paths": [staged_path],
        "existing_draft_id": null,
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/save_draft",
        &draft_request.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "draft save failed: {body}");
    let draft_id = serde_json::from_str::<String>(&body).unwrap();

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_message",
        &json!({ "message_id": draft_id }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "draft read failed: {body}");
    let draft = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(draft["is_draft"], true);
    assert_eq!(draft["has_attachments"], true);

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/delete_draft",
        &json!({ "account_id": account_id, "draft_id": draft_id }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "draft delete failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/cleanup_staged_compose_attachment",
        &json!({ "path": staged_path }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "draft staged cleanup failed: {body}"
    );
}

#[tokio::test]
async fn login_ok_and_rejects_wrong_password() {
    let app = new_app().await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/login",
        r#"{"password":"test-password"}"#,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("token"));

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/login",
        r#"{"password":"wrong"}"#,
        None,
    )
    .await;
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
async fn message_thread_and_search_read_commands_roundtrip_empty_state() {
    let app = new_app().await;
    let token = login_token(&app).await;

    let read_cases = [
        (
            "/api/v1/command/list_messages",
            r#"{"folder_id":"missing-folder","limit":20,"offset":0}"#,
        ),
        (
            "/api/v1/command/list_threads",
            r#"{"folder_id":"missing-folder","limit":20,"offset":0}"#,
        ),
        (
            "/api/v1/command/list_thread_messages",
            r#""missing-thread""#,
        ),
        (
            "/api/v1/command/list_starred_messages",
            r#"{"account_id":"missing-account","limit":20,"offset":0}"#,
        ),
        (
            "/api/v1/command/get_messages_batch",
            r#"{"message_ids":[]}"#,
        ),
        (
            "/api/v1/command/get_message_with_html",
            r#"{"message_id":"missing-message","privacy_mode":"Strict"}"#,
        ),
    ];

    for (path, body) in read_cases {
        let (status, response) = request(&app, "POST", path, body, Some(&token)).await;
        assert_eq!(status, StatusCode::OK, "{path} failed: {response}");
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        if path.ends_with("get_message_with_html") {
            assert!(
                value.is_null(),
                "{path} should return null for a missing message"
            );
        } else {
            assert!(value.is_array(), "{path} should return an array");
            assert!(
                value.as_array().unwrap().is_empty(),
                "{path} should be empty"
            );
        }
    }

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_message",
        r#"{"message_id":"missing-message"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        serde_json::Value::Null
    );

    for (path, request_body) in [
        ("/api/v1/command/search_messages", r#"{"query":""}"#),
        (
            "/api/v1/command/advanced_search",
            r#"{"query":{},"limit":20}"#,
        ),
    ] {
        let (status, response) = request(&app, "POST", path, request_body, Some(&token)).await;
        assert_eq!(status, StatusCode::OK, "{path} failed: {response}");
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.is_array(), "{path} should return an array");
        assert!(
            value.as_array().unwrap().is_empty(),
            "{path} should be empty"
        );
    }
}

#[tokio::test]
async fn message_flags_and_batch_mutations_roundtrip() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let first_id = create_local_draft(&app, &token, "flags").await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_accounts",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let account_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/save_draft",
        &json!({
            "account_id": account_id,
            "to": ["recipient@example.com"],
            "subject": "second flags draft",
            "body_text": "second body"
        })
        .to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "second draft failed: {body}");
    let second_id = serde_json::from_str::<String>(&body).unwrap();

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/update_message_flags",
        &json!({ "message_id": first_id, "is_read": false, "is_starred": true }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "single flag update failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_message",
        &json!({ "message_id": first_id }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let message = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(message["is_read"], false);
    assert_eq!(message["is_starred"], true);

    let ids = json!({ "message_ids": [first_id, second_id] });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/batch_mark_read",
        &json!({ "message_ids": ids["message_ids"], "is_read": true }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "batch mark-read failed: {body}");
    assert_eq!(body, "2");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/batch_star",
        &json!({ "message_ids": ids["message_ids"], "starred": false }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "batch star failed: {body}");
    assert_eq!(body, "2");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/get_messages_batch",
        &ids.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let messages = serde_json::from_str::<Vec<serde_json::Value>>(&body).unwrap();
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|message| message["is_read"] == true));
    assert!(messages
        .iter()
        .all(|message| message["is_starred"] == false));

    for (path, args) in [
        (
            "/api/v1/command/batch_mark_read",
            json!({ "message_ids": ["missing-message"], "is_read": true }),
        ),
        (
            "/api/v1/command/batch_star",
            json!({ "message_ids": ["missing-message"], "starred": true }),
        ),
    ] {
        let (status, body) = request(&app, "POST", path, &args.to_string(), Some(&token)).await;
        assert_eq!(status, StatusCode::OK, "{path} failed: {body}");
        assert_eq!(body, "0", "{path} should report no successful updates");
    }
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
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/add_account",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(body.contains("BAD_REQUEST"));

    // 非法 provider → 400（前端形态：add_account 传 { request: AddAccountRequest }）
    let bad = json!({
        "request": {
            "email": "a@b.com", "display_name": "A", "provider": "gmail",
            "imap_host": "x", "imap_port": 993, "smtp_host": "x", "smtp_port": 465,
            "username": "a", "password": "p", "imap_security": "tls", "smtp_security": "tls"
        }
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/add_account",
        &bad.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(body.contains("not supported yet"));
}

#[tokio::test]
async fn pop3_account_creation_and_update_are_supported() {
    let app = new_app().await;
    let token = login_token(&app).await;
    let request_body = json!({
        "request": {
            "email": "pop3@example.com",
            "display_name": "POP3",
            "provider": "pop3",
            "imap_host": "127.0.0.1",
            "imap_port": 1,
            "smtp_host": "127.0.0.1",
            "smtp_port": 1,
            "username": "pop3",
            "password": "pw",
            "imap_security": "plain",
            "smtp_security": "plain",
            "allow_plaintext": true
        }
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/add_account",
        &request_body.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "POP3 add failed: {body}");
    let account: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(account["provider"], "pop3");
    let account_id = account["id"].as_str().unwrap();

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/test_account_connection",
        &json!({ "account_id": account_id }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body.contains("only supported for IMAP accounts"));

    let update = json!({
        "account_id": account_id,
        "email": "pop3@example.com",
        "display_name": "POP3 updated",
        "imap_host": "127.0.0.1",
        "imap_port": 2,
        "imap_security": "plain",
        "smtp_host": "127.0.0.1",
        "smtp_port": 2,
        "smtp_security": "plain",
        "password": "pw2"
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/update_account",
        &update.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "POP3 update failed: {body}");
}

#[tokio::test]
async fn accounts_crud_roundtrip() {
    let app = new_app().await;
    let token = login_token(&app).await;

    // 与上游 Tauri 命令签名一致：add_account({ request: AddAccountRequest })
    let acc = json!({
        "request": {
            "email": "crud@example.com", "display_name": "Crud", "provider": "imap",
            "imap_host": "imap.example.com", "imap_port": 993, "smtp_host": "smtp.example.com",
            "smtp_port": 465, "username": "crud", "password": "pw",
            "imap_security": "tls", "smtp_security": "tls"
        }
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/add_account",
        &acc.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "add failed: {body}");
    let account: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = account["id"].as_str().unwrap();

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_accounts",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    let update = json!({
        "account_id": id,
        "email": "crud-updated@example.com",
        "display_name": "Crud Updated",
        "imap_host": "imap-updated.example.com",
        "imap_port": 993,
        "smtp_host": "smtp-updated.example.com",
        "smtp_port": 465,
        "imap_security": "tls",
        "smtp_security": "tls",
        "password": "pw-updated"
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/update_account",
        &update.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "IMAP update failed: {body}");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_accounts",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let updated: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(updated[0]["email"], "crud-updated@example.com");
    assert_eq!(updated[0]["display_name"], "Crud Updated");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/stage_compose_attachment",
        r#"{"filename":"account-owned.txt","bytes":[1,2,3]}"#,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stage attachment failed: {body}");
    let staged_path: String = serde_json::from_str(&body).unwrap();
    let draft = json!({
        "account_id": id,
        "to": ["recipient@example.com"],
        "subject": "account cleanup",
        "body_text": "attachment cleanup",
        "attachment_paths": [staged_path]
    });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/save_draft",
        &draft.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "draft attachment failed: {body}");
    let draft_id: String = serde_json::from_str(&body).unwrap();
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_attachments",
        &json!({ "message_id": draft_id }).to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let attachments: serde_json::Value = serde_json::from_str(&body).unwrap();
    let local_attachment_path = attachments[0]["local_path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&local_attachment_path).exists());

    // 删除不存在 → 幂等 200（与 store.delete_account 语义一致：无行删除不报错）
    let (status, _) = request(
        &app,
        "POST",
        "/api/v1/command/delete_account",
        "{\"account_id\":\"no-such-id\"}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let del = json!({ "account_id": id });
    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/delete_account",
        &del.to_string(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete failed: {body}");
    assert!(!std::path::Path::new(&local_attachment_path).exists());

    let (status, body) = request(
        &app,
        "POST",
        "/api/v1/command/list_accounts",
        "{}",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(serde_json::from_str::<serde_json::Value>(&body)
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
}
