use pebble_core::{new_id, now_timestamp, Account, HttpProxyConfig, PebbleError, ProviderType};
use pebble_mail::{ConnectionSecurity, ImapConfig, ProxyConfig, SmtpConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::encrypted_store;
use crate::commands::network::{
    account_proxy_setting_from_parts, http_proxy_from_mail_proxy, is_inherit_proxy_mode,
    mail_proxy_from_http, normalize_account_proxy_setting,
    proxy_config_from_parts as http_proxy_config_from_parts, AccountProxyMode, AccountProxySetting,
};
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 与桌面端 src-tauri/src/commands/accounts.rs 的 AddAccountRequest 字节级对齐。
#[derive(Deserialize)]
pub struct AddAccountRequest {
    #[serde(default)]
    pub account_label: Option<String>,
    pub email: String,
    pub display_name: String,
    pub provider: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub username: String,
    pub password: String,
    pub imap_security: ConnectionSecurity,
    pub smtp_security: ConnectionSecurity,
    #[serde(default)]
    pub accept_invalid_certs: bool,
    #[serde(default)]
    pub allow_plaintext: bool,
    #[serde(default)]
    pub proxy_host: Option<String>,
    #[serde(default)]
    pub proxy_port: Option<u16>,
}

/// 与桌面端 StoredAccountCredentials 字节级一致的存储结构（auth_data 数据格式兼容）。
///
/// 注意：不能用 pebble-mail 的 ImapConfig/SmtpConfig 直接序列化存储——
/// 它们为匹配运行时用途 skip_serializing password，而桌面端会把密码明文
/// 序列化进 auth_data 后整体 AES 加密。此处结构必需显式包含 password。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountCredentials {
    #[serde(default, skip_serializing_if = "is_inherit_proxy_mode")]
    pub proxy_mode: AccountProxyMode,
    pub imap: StoredMailConfig,
    pub smtp: StoredMailConfig,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_plaintext: bool,
}

/// 与桌面端 StoredMailConfig 一致：兼容旧 use_tls 布尔字段的读取。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMailConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub security: Option<ConnectionSecurity>,
    #[serde(default)]
    pub use_tls: Option<bool>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub accept_invalid_certs: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
}

impl StoredMailConfig {
    pub fn security(&self) -> ConnectionSecurity {
        self.security.clone().unwrap_or(match self.use_tls {
            Some(false) => ConnectionSecurity::Plain,
            _ => ConnectionSecurity::Tls,
        })
    }

    pub fn into_smtp(self) -> SmtpConfig {
        let security = self.security();
        SmtpConfig {
            host: self.host,
            port: self.port,
            username: self.username,
            password: self.password,
            security,
            accept_invalid_certs: self.accept_invalid_certs,
            proxy: self.proxy,
        }
    }

    pub fn from_imap(config: ImapConfig) -> Self {
        Self {
            host: config.host,
            port: config.port,
            username: config.username,
            password: config.password,
            security: Some(config.security),
            use_tls: None,
            accept_invalid_certs: config.accept_invalid_certs,
            proxy: config.proxy,
        }
    }

    pub fn from_smtp(config: SmtpConfig) -> Self {
        Self {
            host: config.host,
            port: config.port,
            username: config.username,
            password: config.password,
            security: Some(config.security),
            use_tls: None,
            accept_invalid_certs: config.accept_invalid_certs,
            proxy: config.proxy,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn serialize_account_credentials(
    credentials: &AccountCredentials,
) -> Result<Vec<u8>, PebbleError> {
    serde_json::to_vec(credentials)
        .map_err(|e| PebbleError::Internal(format!("Failed to serialize config: {e}")))
}

fn deserialize_account_credentials(bytes: &[u8]) -> Result<AccountCredentials, PebbleError> {
    serde_json::from_slice(bytes)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse config: {e}")))
}

fn account_proxy_from_credentials(credentials: &AccountCredentials) -> Option<HttpProxyConfig> {
    credentials
        .imap
        .proxy
        .as_ref()
        .or(credentials.smtp.proxy.as_ref())
        .map(http_proxy_from_mail_proxy)
}

fn account_proxy_setting_from_credentials(credentials: &AccountCredentials) -> AccountProxySetting {
    normalize_account_proxy_setting(
        credentials.proxy_mode,
        account_proxy_from_credentials(credentials),
    )
}

fn set_account_proxy_setting_on_credentials(
    credentials: &mut AccountCredentials,
    setting: AccountProxySetting,
) {
    credentials.proxy_mode = setting.mode;
    let proxy = setting.proxy.map(mail_proxy_from_http);
    credentials.imap.proxy = proxy.clone();
    credentials.smtp.proxy = proxy;
}

/// 与桌面端一致的明文连接安全校验。
pub(crate) fn validate_connection_security(
    label: &str,
    host: &str,
    security: &ConnectionSecurity,
    allow_plaintext: bool,
) -> Result<(), PebbleError> {
    if matches!(security, ConnectionSecurity::Plain)
        && !is_loopback_mail_host(host)
        && !allow_plaintext
    {
        return Err(PebbleError::Validation(format!(
            "{label} plaintext connections are disabled by default. Enable \"Allow \
             unencrypted connection\" for this account to connect to a server that only \
             supports plaintext (your password will be sent unencrypted)."
        )));
    }
    Ok(())
}

fn is_loopback_mail_host(host: &str) -> bool {
    matches!(
        host.trim()
            .trim_matches(&['[', ']'][..])
            .to_ascii_lowercase()
            .as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

fn resolve_username(username: &str, email: &str) -> String {
    if username.is_empty() {
        email.to_string()
    } else {
        username.to_string()
    }
}

pub(crate) fn mail_proxy_config_from_parts(
    host: Option<String>,
    port: Option<u16>,
    label: &str,
) -> Result<Option<ProxyConfig>, PebbleError> {
    Ok(http_proxy_config_from_parts(host, port, label)?.map(mail_proxy_from_http))
}

pub async fn list_accounts(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let accounts = state.store.list_accounts().map_err(ApiError::from_store)?;
    serde_json::to_value(accounts).map_err(ApiError::from_serialize)
}

#[derive(Deserialize)]
struct AccountProxyArgs {
    account_id: String,
}

pub async fn get_account_proxy(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: AccountProxyArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_account_proxy args: {e}")))?;
    let setting = get_account_proxy_setting_value(&state, &args.account_id)?;
    serde_json::to_value(setting.proxy).map_err(ApiError::from_serialize)
}

pub async fn get_account_proxy_setting(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: AccountProxyArgs = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid get_account_proxy_setting args: {e}"))
    })?;
    serde_json::to_value(get_account_proxy_setting_value(&state, &args.account_id)?)
        .map_err(ApiError::from_serialize)
}

fn get_account_proxy_setting_value(
    state: &AppStateRef,
    account_id: &str,
) -> Result<AccountProxySetting, ApiError> {
    let account = state
        .store
        .get_account(account_id)
        .map_err(ApiError::from_store)?
        .ok_or_else(|| PebbleError::Internal(format!("Account not found: {account_id}")))?;
    if !matches!(account.provider, ProviderType::Imap | ProviderType::Pop3) {
        return Err(ApiError::from_pebble(PebbleError::UnsupportedProvider(
            "Use the OAuth account proxy commands for Gmail and Outlook accounts".to_string(),
        )));
    }
    let Some(bytes) = encrypted_store::load_account_auth_data(&state.crypto, &state.store, account_id)
        .map_err(ApiError::from_pebble)?
    else {
        return Ok(AccountProxySetting {
            mode: AccountProxyMode::Inherit,
            proxy: None,
        });
    };
    let credentials = deserialize_account_credentials(&bytes)?;
    Ok(account_proxy_setting_from_credentials(&credentials))
}

#[derive(Deserialize)]
struct UpdateAccountProxyArgs {
    account_id: String,
    #[serde(default)]
    proxy_host: Option<String>,
    #[serde(default)]
    proxy_port: Option<u16>,
}

pub async fn update_account_proxy(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: UpdateAccountProxyArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid update_account_proxy args: {e}")))?;
    let proxy = mail_proxy_config_from_parts(args.proxy_host, args.proxy_port, "Account proxy")
        .map_err(ApiError::from_pebble)?;
    let setting = AccountProxySetting {
        mode: if proxy.is_some() {
            AccountProxyMode::Custom
        } else {
            AccountProxyMode::Inherit
        },
        proxy: proxy.map(|proxy| HttpProxyConfig {
            host: proxy.host,
            port: proxy.port,
        }),
    };
    update_account_proxy_setting_value(&state, &args.account_id, setting)?;
    Ok(Value::Null)
}

#[derive(Deserialize)]
struct UpdateAccountProxySettingArgs {
    account_id: String,
    mode: AccountProxyMode,
    #[serde(default)]
    proxy_host: Option<String>,
    #[serde(default)]
    proxy_port: Option<u16>,
}

pub async fn update_account_proxy_setting(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    let args: UpdateAccountProxySettingArgs = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid update_account_proxy_setting args: {e}"))
    })?;
    let setting = account_proxy_setting_from_parts(
        args.mode,
        args.proxy_host,
        args.proxy_port,
        "Account proxy",
    )
    .map_err(ApiError::from_pebble)?;
    update_account_proxy_setting_value(&state, &args.account_id, setting)?;
    Ok(Value::Null)
}

fn update_account_proxy_setting_value(
    state: &AppStateRef,
    account_id: &str,
    setting: AccountProxySetting,
) -> Result<(), ApiError> {
    let account = state
        .store
        .get_account(account_id)
        .map_err(ApiError::from_store)?
        .ok_or_else(|| PebbleError::Internal(format!("Account not found: {account_id}")))?;
    if !matches!(account.provider, ProviderType::Imap | ProviderType::Pop3) {
        return Err(ApiError::from_pebble(PebbleError::UnsupportedProvider(
            "Use the OAuth account proxy commands for Gmail and Outlook accounts".to_string(),
        )));
    }

    let Some(bytes) = encrypted_store::load_account_auth_data(&state.crypto, &state.store, account_id)
        .map_err(ApiError::from_pebble)?
    else {
        return Err(PebbleError::Internal(format!(
            "No auth data found for account {account_id}"
        ))
        .into());
    };
    let mut credentials = deserialize_account_credentials(&bytes)?;
    set_account_proxy_setting_on_credentials(&mut credentials, setting);
    let serialized = serialize_account_credentials(&credentials)?;
    encrypted_store::store_account_auth_data(&state.crypto, &state.store, account_id, &serialized)
        .map_err(ApiError::from_pebble)
}

/// 创建邮件账户（IMAP/SMTP 或 POP3/SMTP）。OAuth 提供器走独立浏览器流程。
pub async fn add_account(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    // 与上游 Tauri 命令签名一致：前端 invoke 传 { request: AddAccountRequest }
    let request: AddAccountRequest =
        serde_json::from_value(args.get("request").cloned().unwrap_or(Value::Null))
            .map_err(|e| ApiError::BadRequest(format!("invalid add_account args: {e}")))?;
    pebble_mail::sender::sender_mailbox(&pebble_core::EmailAddress {
        name: Some(request.display_name.clone()),
        address: request.email.clone(),
    })?;
    let provider = match request.provider.to_lowercase().as_str() {
        "imap" => ProviderType::Imap,
        "pop3" => ProviderType::Pop3,
        other => {
            return Err(ApiError::BadRequest(format!(
                "provider {other} not supported yet"
            )));
        }
    };

    let incoming_label = if matches!(provider, ProviderType::Pop3) {
        "POP3"
    } else {
        "IMAP"
    };
    validate_connection_security(
        incoming_label,
        &request.imap_host,
        &request.imap_security,
        request.allow_plaintext,
    )
    .map_err(ApiError::from_pebble)?;
    validate_connection_security(
        "SMTP",
        &request.smtp_host,
        &request.smtp_security,
        request.allow_plaintext,
    )
    .map_err(ApiError::from_pebble)?;

    let now = now_timestamp();
    let existing_accounts = state.store.list_accounts().map_err(ApiError::from_store)?;
    let account = Account {
        account_label: pebble_store::accounts::normalize_account_label(
            request.account_label.as_deref(),
        )?,
        provider_display_name: None,
        id: new_id(),
        email: request.email.clone(),
        display_name: request.display_name.clone(),
        color: Some(crate::account_colors::default_account_color(&existing_accounts, &request.email)),
        provider: provider.clone(),
        created_at: now,
        updated_at: now,
    };

    state
        .store
        .insert_account(&account)
        .map_err(ApiError::from_store)?;

    // 后续步骤失败则回滚账户行，避免半成品账户（与桌面端一致）
    if let Err(e) = (|| -> Result<(), PebbleError> {
        let proxy =
            mail_proxy_config_from_parts(request.proxy_host, request.proxy_port, "Account proxy")?;
        let proxy_mode = if proxy.is_some() {
            AccountProxyMode::Custom
        } else {
            AccountProxyMode::Inherit
        };

        let username = resolve_username(&request.username, &request.email);

        let credentials = AccountCredentials {
            proxy_mode,
            imap: StoredMailConfig::from_imap(ImapConfig {
                host: request.imap_host,
                port: request.imap_port,
                username: username.clone(),
                password: request.password.clone(),
                security: request.imap_security,
                accept_invalid_certs: request.accept_invalid_certs,
                proxy: proxy.clone(),
            }),
            smtp: StoredMailConfig::from_smtp(SmtpConfig {
                host: request.smtp_host,
                port: request.smtp_port,
                username,
                password: request.password,
                security: request.smtp_security,
                accept_invalid_certs: request.accept_invalid_certs,
                proxy,
            }),
            allow_plaintext: request.allow_plaintext,
        };

        let config_bytes = serialize_account_credentials(&credentials)?;
        encrypted_store::store_account_auth_data(
            &state.crypto,
            &state.store,
            &account.id,
            &config_bytes,
        )?;

        state.store.update_sync_state(&account.id, |s| {
            s.provider = Some(if matches!(provider, ProviderType::Pop3) {
                "pop3".to_string()
            } else {
                "imap".to_string()
            });
            if matches!(provider, ProviderType::Imap) {
                // 新 IMAP 账户默认只同步收件箱，其余邮箱由账户编辑器按需开启
                s.selected_imap_folder_remote_ids = Some(Vec::new());
            }
        })?;

        Ok(())
    })() {
        let _ = state.store.delete_account(&account.id);
        return Err(ApiError::from_pebble(e));
    }

    serde_json::to_value(account).map_err(ApiError::from_serialize)
}

/// 更新账户（参数对齐桌面端 update_account：全 Option 字段，email/display_name 必填）。
/// 凭据相关字段任一变更即触发合并改写 auth_data；仅元数据变更则直接更新行。
#[derive(Deserialize)]
pub struct UpdateAccountRequest {
    #[serde(default)]
    pub account_label: Option<String>,
    pub account_id: String,
    pub email: String,
    pub display_name: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub imap_host: Option<String>,
    #[serde(default)]
    pub imap_port: Option<u16>,
    #[serde(default)]
    pub smtp_host: Option<String>,
    #[serde(default)]
    pub smtp_port: Option<u16>,
    #[serde(default)]
    pub imap_security: Option<ConnectionSecurity>,
    #[serde(default)]
    pub smtp_security: Option<ConnectionSecurity>,
    #[serde(default)]
    pub accept_invalid_certs: Option<bool>,
    #[serde(default)]
    pub proxy_host: Option<String>,
    #[serde(default)]
    pub proxy_port: Option<u16>,
    #[serde(default)]
    pub account_color: Option<String>,
}

pub async fn update_account(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let req: UpdateAccountRequest = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid update_account args: {e}")))?;

    validate_account_color(req.account_color.as_deref())?;
    pebble_mail::sender::sender_mailbox(&pebble_core::EmailAddress {
        name: Some(req.display_name.clone()),
        address: req.email.clone(),
    })?;
    pebble_store::accounts::normalize_account_label(req.account_label.as_deref())?;

    let credentials_dirty = req.password.is_some()
        || req.imap_host.is_some()
        || req.smtp_host.is_some()
        || req.imap_port.is_some()
        || req.smtp_port.is_some()
        || req.imap_security.is_some()
        || req.smtp_security.is_some()
        || req.accept_invalid_certs.is_some()
        || req.proxy_host.is_some()
        || req.proxy_port.is_some();

    if !credentials_dirty {
        state
            .store
            .update_account_details(
                &req.account_id,
                &req.email,
                &req.display_name,
                req.account_color.as_deref(),
                req.account_label.as_deref(),
            )
            .map_err(ApiError::from_store)?;
        return Ok(Value::Null);
    }

    let provider = state
        .store
        .get_account(&req.account_id)?
        .map(|account| account.provider)
        .ok_or_else(|| PebbleError::Internal(format!("Account not found: {}", req.account_id)))?;

    // 解析现有凭据；缺失时（首次编辑或 OAuth 旧账户）以空模板开始（与桌面端一致）
    let mut creds =
        match encrypted_store::load_account_auth_data(&state.crypto, &state.store, &req.account_id)? {
            Some(bytes) => deserialize_account_credentials(&bytes)?,
            None => AccountCredentials {
                proxy_mode: AccountProxyMode::Inherit,
                imap: StoredMailConfig {
                    host: String::new(),
                    port: 0,
                    username: String::new(),
                    password: String::new(),
                    security: None,
                    use_tls: None,
                    accept_invalid_certs: false,
                    proxy: None,
                },
                smtp: StoredMailConfig {
                    host: String::new(),
                    port: 0,
                    username: String::new(),
                    password: String::new(),
                    security: None,
                    use_tls: None,
                    accept_invalid_certs: false,
                    proxy: None,
                },
                allow_plaintext: false,
            },
        };

    let updated_proxy = if req.proxy_host.is_some() || req.proxy_port.is_some() {
        Some(
            mail_proxy_config_from_parts(req.proxy_host.clone(), req.proxy_port, "Account proxy")
                .map_err(ApiError::from_pebble)?,
        )
    } else {
        None
    };

    if let Some(h) = req.imap_host {
        creds.imap.host = h;
    }
    if let Some(p) = req.imap_port {
        creds.imap.port = p;
    }
    if let Some(ref pw) = req.password {
        creds.imap.password = pw.clone();
    }
    if let Some(sec) = req.imap_security {
        creds.imap.security = Some(sec);
    }
    if let Some(acc) = req.accept_invalid_certs {
        creds.imap.accept_invalid_certs = acc;
    }
    if let Some(proxy) = &updated_proxy {
        creds.proxy_mode = if proxy.is_some() {
            AccountProxyMode::Custom
        } else {
            AccountProxyMode::Inherit
        };
        creds.imap.proxy = proxy.clone();
    }
    creds.imap.username = resolve_username(&creds.imap.username, &req.email);

    if let Some(h) = req.smtp_host {
        creds.smtp.host = h;
    }
    if let Some(p) = req.smtp_port {
        creds.smtp.port = p;
    }
    if let Some(ref pw) = req.password {
        creds.smtp.password = pw.clone();
    }
    if let Some(sec) = req.smtp_security {
        creds.smtp.security = Some(sec);
    }
    if let Some(acc) = req.accept_invalid_certs {
        creds.smtp.accept_invalid_certs = acc;
    }
    if let Some(proxy) = updated_proxy {
        creds.smtp.proxy = proxy;
    }
    creds.smtp.username = resolve_username(&creds.smtp.username, &req.email);

    // allow_plaintext 仅创建时设置；编辑时由 反序列化→序列化 往返保留（与桌面端一致）
    let incoming_label = if provider == ProviderType::Pop3 {
        "POP3"
    } else {
        "IMAP"
    };
    validate_connection_security(
        incoming_label,
        &creds.imap.host,
        &creds.imap.security(),
        creds.allow_plaintext,
    )
    .map_err(ApiError::from_pebble)?;
    validate_connection_security(
        "SMTP",
        &creds.smtp.host,
        &creds.smtp.security(),
        creds.allow_plaintext,
    )
    .map_err(ApiError::from_pebble)?;

    state
        .store
        .update_account_details(
            &req.account_id,
            &req.email,
            &req.display_name,
            req.account_color.as_deref(),
            req.account_label.as_deref(),
        )
        .map_err(ApiError::from_store)?;

    let config_bytes = serialize_account_credentials(&creds)?;
    encrypted_store::store_account_auth_data(
        &state.crypto,
        &state.store,
        &req.account_id,
        &config_bytes,
    )?;
    Ok(Value::Null)
}

fn validate_account_color(color: Option<&str>) -> Result<(), PebbleError> {
    let Some(color) = color else {
        return Ok(());
    };

    if color.len() == 7
        && color.as_bytes()[0] == b'#'
        && color.as_bytes()[1..].iter().all(|b| b.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(PebbleError::Validation(
            "Account color must be a hex color like #22c55e".to_string(),
        ))
    }
}

#[derive(Deserialize)]
pub struct DeleteAccountRequest {
    pub account_id: String,
}

pub async fn delete_account(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let req: DeleteAccountRequest = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid delete_account args: {e}")))?;

    let _ = state.sync_manager.stop_account(&req.account_id).await;
    let message_ids = match state.store.list_message_ids_by_account(&req.account_id) {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(
                account_id = %req.account_id,
                "Failed to collect message IDs for attachment cleanup: {error}"
            );
            Vec::new()
        }
    };

    // Search cleanup is best-effort, matching the desktop adapter. A stale
    // Tantivy document must not prevent the authoritative account row from
    // being deleted, and reporting an error after the DB delete would leave
    // the caller with a misleading partial-success result.
    if let Err(error) = state.search.delete_by_account(&req.account_id) {
        tracing::warn!(
            account_id = %req.account_id,
            "Failed to clean search index while deleting account: {error}"
        );
    }

    state
        .store
        .delete_account(&req.account_id)
        .map_err(ApiError::from_store)?;

    let attachments_dir = state.attachments_dir.clone();
    if let Err(error) = tokio::task::spawn_blocking(move || {
        for message_id in &message_ids {
            let message_dir = attachments_dir.join(message_id);
            if message_dir.exists() {
                if let Err(error) = std::fs::remove_dir_all(&message_dir) {
                    tracing::warn!(
                        "Failed to remove attachments for message {message_id}: {error}"
                    );
                }
            }
        }
    })
    .await
    {
        tracing::warn!(
            account_id = %req.account_id,
            "Attachment cleanup task failed after account deletion: {error}"
        );
    }

    Ok(Value::Null)
}
mod connection;

pub(crate) use connection::{test_account_connection, test_imap_connection, test_pop3_connection};
