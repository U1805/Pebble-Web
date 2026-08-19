use std::collections::HashSet;

use pebble_core::{now_timestamp, new_id, Account, PebbleError, ProviderType};
use pebble_mail::{ConnectionSecurity, ImapConfig, ProxyConfig, SmtpConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::credentials;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 与桌面端 src-tauri/commands/accounts.rs 的 AddAccountRequest 字节级对齐。
#[derive(Deserialize)]
pub struct AddAccountRequest {
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

/// 与桌面端 enum 序列化兼容（lowercase，默认 Inherit）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AccountProxyMode {
    #[default]
    Inherit,
    Disabled,
    Custom,
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

fn is_inherit_proxy_mode(mode: &AccountProxyMode) -> bool {
    matches!(mode, AccountProxyMode::Inherit)
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// 与桌面端 account_colors.rs 完全一致的取色逻辑（12 色预设 + 稳定 hash）。
const ACCOUNT_COLOR_PRESETS: [&str; 12] = [
    "#0ea5e9", "#22c55e", "#f59e0b", "#8b5cf6", "#f43f5e", "#14b8a6", "#6366f1", "#f97316",
    "#06b6d4", "#ec4899", "#84cc16", "#3b82f6",
];

fn default_account_color(existing_accounts: &[Account], seed: &str) -> String {
    let used_colors: HashSet<String> = existing_accounts
        .iter()
        .filter_map(|account| account.color.as_deref())
        .filter(|color| {
            color.len() == 7
                && color.as_bytes()[0] == b'#'
                && color.as_bytes()[1..].iter().all(|b| b.is_ascii_hexdigit())
        })
        .map(str::to_ascii_lowercase)
        .collect();

    if let Some(color) = ACCOUNT_COLOR_PRESETS
        .iter()
        .find(|color| !used_colors.contains(**color))
    {
        return (*color).to_string();
    }

    let mut hash = 0u32;
    for byte in seed.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u32);
    }
    ACCOUNT_COLOR_PRESETS[(hash as usize) % ACCOUNT_COLOR_PRESETS.len()].to_string()
}

/// 与桌面端一致的明文连接安全校验。
fn validate_connection_security(
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
        host.trim().trim_matches(&['[', ']'][..]).to_ascii_lowercase().as_str(),
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

fn proxy_config_from_parts(
    host: Option<String>,
    port: Option<u16>,
    label: &str,
) -> Result<Option<ProxyConfig>, PebbleError> {
    match (host, port) {
        (Some(h), Some(p)) if !h.trim().is_empty() && p != 0 => {
            Ok(Some(ProxyConfig { host: h.trim().to_string(), port: p }))
        }
        (None, None) => Ok(None),
        _ => Err(PebbleError::Validation(format!(
            "{label} requires both proxy host and port"
        ))),
    }
}

pub async fn list_accounts(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let accounts = state.store.list_accounts().map_err(ApiError::from_store)?;
    serde_json::to_value(accounts).map_err(ApiError::from_serialize)
}

/// 创建邮件账户（P0：IMAP/SMTP）。Gmail/Outlook/POP3 提供器后续批次接入。
pub async fn add_account(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let request: AddAccountRequest = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid add_account args: {e}")))?;
    let provider = match request.provider.to_lowercase().as_str() {
        "imap" => ProviderType::Imap,
        other => {
            return Err(ApiError::BadRequest(format!(
                "provider {other} not supported yet"
            )));
        }
    };

    validate_connection_security(
        "IMAP",
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
        id: new_id(),
        email: request.email.clone(),
        display_name: request.display_name.clone(),
        color: Some(default_account_color(&existing_accounts, &request.email)),
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
        let proxy = proxy_config_from_parts(request.proxy_host, request.proxy_port, "Account proxy")?;
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

        let config_bytes =
            serde_json::to_vec(&credentials).map_err(|e| PebbleError::Internal(e.to_string()))?;
        credentials::store_account_auth_data(&state.crypto, &state.store, &account.id, &config_bytes)
            .map_err(|e| PebbleError::Internal(e.to_string()))?;

        state
            .store
            .update_sync_state(&account.id, |s| {
                s.provider = Some("imap".to_string());
                // 新 IMAP 账户默认只同步收件箱，其余邮箱由账户编辑器按需开启
                s.selected_imap_folder_remote_ids = Some(Vec::new());
            })
            .map_err(|e| PebbleError::Internal(e.to_string()))?;

        Ok(())
    })() {
        let _ = state.store.delete_account(&account.id);
        return Err(ApiError::from_pebble(e));
    }

    // P0：新增账户后立即触发首次同步（后台执行，不等同完成）
    let sync = state.sync_manager.clone();
    let account_for_sync = account.clone();
    tokio::spawn(async move {
        if let Err(e) = sync.sync_account(&account_for_sync).await {
            tracing::warn!("initial sync failed for {}: {e}", account_for_sync.id);
        }
    });

    serde_json::to_value(account).map_err(ApiError::from_serialize)
}

/// 更新账户（参数对齐桌面端 update_account：全 Option 字段，email/display_name 必填）。
/// 凭据相关字段任一变更即触发合并改写 auth_data；仅元数据变更则直接更新行。
#[derive(Deserialize)]
pub struct UpdateAccountRequest {
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

    state
        .store
        .update_account(
            &req.account_id,
            &req.email,
            &req.display_name,
            req.account_color.as_deref(),
        )
        .map_err(ApiError::from_store)?;
    if !credentials_dirty {
        return Ok(Value::Null);
    }

    let provider = state
        .store
        .get_account(&req.account_id)
        .map_err(ApiError::from_store)?
        .map(|a| a.provider)
        .ok_or_else(|| ApiError::NotFound("account not found".to_string()))?;

    // 解析现有凭据；缺失时（首次编辑或 OAuth 旧账户）以空模板开始（与桌面端一致）
    let mut creds = match credentials::load_account_auth_data(
        &state.crypto,
        &state.store,
        &req.account_id,
    )? {
        Some(bytes) => serde_json::from_slice::<AccountCredentials>(&bytes)
            .map_err(|e| ApiError::Internal(format!("failed to parse stored credentials: {e}")))?,
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
            proxy_config_from_parts(
                req.proxy_host.clone(),
                req.proxy_port,
                "Account proxy",
            )
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
    let incoming_label = if provider == ProviderType::Pop3 { "POP3" } else { "IMAP" };
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

    let config_bytes = serde_json::to_vec(&creds)
        .map_err(|e| ApiError::Internal(format!("failed to serialize credentials: {e}")))?;
    credentials::store_account_auth_data(
        &state.crypto,
        &state.store,
        &req.account_id,
        &config_bytes,
    )?;
    Ok(Value::Null)
}

fn validate_account_color(color: Option<&str>) -> Result<(), ApiError> {
    let Some(color) = color else { return Ok(()) };
    if color.len() == 7
        && color.as_bytes()[0] == b'#'
        && color.as_bytes()[1..].iter().all(|b| b.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "INVALID_ARGUMENT: account color must be a hex color like #22c55e".to_string(),
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
    // TODO: 同步句柄停止 + 消息附件清理，待同步服务接入后补齐
    state
        .store
        .delete_account(&req.account_id)
        .map_err(ApiError::from_store)?;
    credentials::clear_account_auth_data(&state.store, &req.account_id)?;
    Ok(Value::Null)
}