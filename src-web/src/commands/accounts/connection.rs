use pebble_core::traits::FolderProvider;
use pebble_core::{PebbleError, ProviderType};
use pebble_mail::{ConnectionSecurity, ImapConfig, ImapProvider, Pop3Config, Pop3Provider};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::commands::accounts::validate_connection_security;
use crate::commands::encrypted_store::load_account_auth_data;
use crate::commands::network::{
    get_global_proxy_raw, mail_proxy_from_http, proxy_config_from_parts, resolve_effective_proxy,
    resolve_mail_proxy_from_mode,
};
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 与桌面端 TestConnectionRequest 字段对齐（snake_case）。
#[derive(Deserialize)]
pub struct TestConnectionRequest {
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_security: ConnectionSecurity,
    #[serde(default)]
    pub accept_invalid_certs: bool,
    #[serde(default)]
    pub proxy_host: Option<String>,
    #[serde(default)]
    pub proxy_port: Option<u16>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub allow_plaintext: bool,
}

/// 与桌面端 TestPop3ConnectionRequest 字段对齐（snake_case）。
#[derive(Deserialize)]
pub struct TestPop3ConnectionRequest {
    pub pop3_host: String,
    pub pop3_port: u16,
    pub pop3_security: ConnectionSecurity,
    #[serde(default)]
    pub accept_invalid_certs: bool,
    #[serde(default)]
    pub proxy_host: Option<String>,
    #[serde(default)]
    pub proxy_port: Option<u16>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub allow_plaintext: bool,
}

/// 新建账户表单的连接测试：按参数实际连接 IMAP 服务器。
/// 参数：{ request: {...} }（tauri 命令参数 request，结构按桌面 TestConnectionRequest serde）。
/// 含凭据时做登录验证，否则仅验证连通性。
/// 核心连接错误保留与桌面端相同的 PebbleError 语义，再由 Web transport 映射为 HTTP 响应。
pub async fn test_imap_connection(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let request = args.get("request").cloned().ok_or_else(|| {
        ApiError::BadRequest("invalid test_imap_connection args: expected request".into())
    })?;
    let req: TestConnectionRequest = serde_json::from_value(request)
        .map_err(|e| ApiError::BadRequest(format!("invalid test_imap_connection args: {e}")))?;

    validate_connection_security(
        "IMAP",
        &req.imap_host,
        &req.imap_security,
        req.allow_plaintext,
    )?;

    let requested_proxy =
        proxy_config_from_parts(req.proxy_host, req.proxy_port, "Connection test proxy")?;
    let proxy = resolve_effective_proxy(
        requested_proxy,
        get_global_proxy_raw(&state.crypto, &state.store)?,
    )
    .map(mail_proxy_from_http);

    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    // 登录名默认回退到邮箱（与 add_account 的 resolve_username 同语义）
    let login = if username.is_empty() {
        req.email.unwrap_or_default()
    } else {
        username
    };

    let config = ImapConfig {
        host: req.imap_host,
        port: req.imap_port,
        username: login,
        password,
        security: req.imap_security,
        accept_invalid_certs: req.accept_invalid_certs,
        proxy,
    };

    let report = if !config.username.is_empty() && !config.password.is_empty() {
        ImapProvider::test_connection_with_login(&config).await?
    } else {
        ImapProvider::test_connection(&config).await?
    };

    Ok(json!(report))
}

/// 新建 POP3 账户表单的连接测试：必须使用凭据完成 POP3 登录。
/// 参数：{ request: {...} }（字段与桌面端命令保持一致）。
pub async fn test_pop3_connection(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let request = args.get("request").cloned().ok_or_else(|| {
        ApiError::BadRequest("invalid test_pop3_connection args: expected request".into())
    })?;
    let req: TestPop3ConnectionRequest = serde_json::from_value(request)
        .map_err(|e| ApiError::BadRequest(format!("invalid test_pop3_connection args: {e}")))?;

    validate_connection_security(
        "POP3",
        &req.pop3_host,
        &req.pop3_security,
        req.allow_plaintext,
    )?;

    let requested_proxy =
        proxy_config_from_parts(req.proxy_host, req.proxy_port, "Connection test proxy")?;
    let proxy = resolve_effective_proxy(
        requested_proxy,
        get_global_proxy_raw(&state.crypto, &state.store)?,
    )
    .map(mail_proxy_from_http);
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    if username.is_empty() || password.is_empty() {
        return Err(PebbleError::Auth(
            "POP3 username and password are required for connection test".to_string(),
        )
        .into());
    }

    let config = Pop3Config {
        host: req.pop3_host,
        port: req.pop3_port,
        username,
        password,
        security: req.pop3_security,
        accept_invalid_certs: req.accept_invalid_certs,
        proxy,
    };
    let report = Pop3Provider::test_connection(&config).await?;
    Ok(json!(report))
}

/// 已存在账户的连接测试：读取 auth_data 中保存的 IMAP 配置并做登录验证。
/// 参数：{ account_id }（调用层已由前端 accountId 转换）。
pub async fn test_account_connection(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let account_id = args
        .get("account_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ApiError::BadRequest("invalid test_account_connection args: expected account_id".into())
        })?
        .to_string();

    let account = state
        .store
        .get_account(&account_id)?
        .ok_or_else(|| PebbleError::Internal(format!("Account not found: {account_id}")))?;

    let report = match account.provider {
        ProviderType::Imap => {
            let decrypted = load_account_auth_data(&state.crypto, &state.store, &account_id)?
                .ok_or_else(|| PebbleError::Internal("No auth data found".into()))?;
            let credentials = super::deserialize_account_credentials(&decrypted)?;
            let proxy = resolve_mail_proxy_from_mode(
                &state.crypto,
                &state.store,
                credentials.proxy_mode,
                credentials.imap.proxy.clone(),
            )?;
            let security = credentials.imap.security();
            let config = ImapConfig {
                host: credentials.imap.host,
                port: credentials.imap.port,
                username: credentials.imap.username,
                password: credentials.imap.password,
                security,
                accept_invalid_certs: credentials.imap.accept_invalid_certs,
                proxy,
            };
            ImapProvider::test_connection_with_login(&config).await?
        }
        ProviderType::Pop3 => {
            let decrypted = load_account_auth_data(&state.crypto, &state.store, &account_id)?
                .ok_or_else(|| PebbleError::Internal("No auth data found".into()))?;
            let credentials = super::deserialize_account_credentials(&decrypted)?;
            let proxy = resolve_mail_proxy_from_mode(
                &state.crypto,
                &state.store,
                credentials.proxy_mode,
                credentials.imap.proxy.clone(),
            )?;
            let security = credentials.imap.security();
            let config = Pop3Config {
                host: credentials.imap.host,
                port: credentials.imap.port,
                username: credentials.imap.username,
                password: credentials.imap.password,
                security,
                accept_invalid_certs: credentials.imap.accept_invalid_certs,
                proxy,
            };
            Pop3Provider::test_connection(&config).await?
        }
        ProviderType::Gmail => {
            let provider = crate::commands::messages::connect_gmail(&state, &account_id).await?;
            let (email, _history_id) = provider.get_profile().await?;
            if email.is_empty() {
                "Gmail connection successful".to_string()
            } else {
                format!("Gmail connection successful ({email})")
            }
        }
        ProviderType::Outlook => {
            let provider = crate::commands::messages::connect_outlook(&state, &account_id).await?;
            let folders = provider.list_folders().await?;
            format!("Outlook connection successful ({} folders)", folders.len())
        }
    };

    Ok(json!(report))
}
