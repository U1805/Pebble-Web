use std::collections::HashSet;
use std::path::{Path, PathBuf};

use pebble_core::{new_id, now_timestamp, Account, HttpProxyConfig, PebbleError, ProviderType};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountProxySetting {
    pub mode: AccountProxyMode,
    pub proxy: Option<HttpProxyConfig>,
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

fn account_proxy_from_credentials(credentials: &AccountCredentials) -> Option<HttpProxyConfig> {
    credentials
        .imap
        .proxy
        .as_ref()
        .or(credentials.smtp.proxy.as_ref())
        .map(|proxy| HttpProxyConfig {
            host: proxy.host.clone(),
            port: proxy.port,
        })
}

fn account_proxy_setting_from_credentials(credentials: &AccountCredentials) -> AccountProxySetting {
    let proxy = account_proxy_from_credentials(credentials);
    let mode = if matches!(credentials.proxy_mode, AccountProxyMode::Inherit) && proxy.is_some() {
        AccountProxyMode::Custom
    } else {
        credentials.proxy_mode
    };
    AccountProxySetting {
        proxy: if matches!(mode, AccountProxyMode::Custom) {
            proxy
        } else {
            None
        },
        mode,
    }
}

fn set_account_proxy_setting_on_credentials(
    credentials: &mut AccountCredentials,
    setting: AccountProxySetting,
) {
    credentials.proxy_mode = setting.mode;
    let proxy = setting.proxy.map(|proxy| ProxyConfig {
        host: proxy.host,
        port: proxy.port,
    });
    credentials.imap.proxy = proxy.clone();
    credentials.smtp.proxy = proxy;
}

/// 与桌面端 account_colors.rs 完全一致的取色逻辑（12 色预设 + 稳定 hash）。
const ACCOUNT_COLOR_PRESETS: [&str; 12] = [
    "#0ea5e9", "#22c55e", "#f59e0b", "#8b5cf6", "#f43f5e", "#14b8a6", "#6366f1", "#f97316",
    "#06b6d4", "#ec4899", "#84cc16", "#3b82f6",
];

pub(crate) fn default_account_color(existing_accounts: &[Account], seed: &str) -> String {
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

pub(crate) fn proxy_config_from_parts(
    host: Option<String>,
    port: Option<u16>,
    label: &str,
) -> Result<Option<ProxyConfig>, PebbleError> {
    match (host, port) {
        (None, None) => Ok(None),
        (Some(host), None) if host.trim().is_empty() => Ok(None),
        (Some(_), None) => Err(PebbleError::Validation(format!(
            "{label} port is required when proxy host is set"
        ))),
        (None, Some(_)) => Err(PebbleError::Validation(format!(
            "{label} host is required when proxy port is set"
        ))),
        (Some(host), Some(port)) => {
            let proxy = HttpProxyConfig {
                host: host.trim().to_string(),
                port,
            };
            proxy.validate().map_err(PebbleError::Validation)?;
            Ok(Some(ProxyConfig {
                host: proxy.host,
                port: proxy.port,
            }))
        }
    }
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
        .ok_or_else(|| ApiError::NotFound(format!("account not found: {account_id}")))?;
    if !matches!(account.provider, ProviderType::Imap | ProviderType::Pop3) {
        return Err(ApiError::from_pebble(PebbleError::UnsupportedProvider(
            "Use the OAuth account proxy commands for Gmail and Outlook accounts".to_string(),
        )));
    }
    let Some(bytes) = credentials::load_account_auth_data(&state.crypto, &state.store, account_id)
        .map_err(ApiError::Internal)?
    else {
        return Ok(AccountProxySetting {
            mode: AccountProxyMode::Inherit,
            proxy: None,
        });
    };
    let credentials: AccountCredentials = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::Internal(format!("failed to parse stored credentials: {e}")))?;
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
    let proxy = proxy_config_from_parts(args.proxy_host, args.proxy_port, "Account proxy")
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
    let proxy = proxy_config_from_parts(args.proxy_host, args.proxy_port, "Account proxy")
        .map_err(ApiError::from_pebble)?;
    let proxy = match args.mode {
        AccountProxyMode::Custom => Some(proxy.ok_or_else(|| {
            ApiError::BadRequest("custom account proxy requires host and port".to_string())
        })?),
        AccountProxyMode::Inherit | AccountProxyMode::Disabled => None,
    };
    let proxy = proxy.map(|proxy| HttpProxyConfig {
        host: proxy.host,
        port: proxy.port,
    });
    update_account_proxy_setting_value(
        &state,
        &args.account_id,
        AccountProxySetting {
            mode: args.mode,
            proxy,
        },
    )?;
    Ok(Value::Null)
}

fn update_account_proxy_setting_value(
    state: &AppStateRef,
    account_id: &str,
    setting: AccountProxySetting,
) -> Result<(), ApiError> {
    let current = get_account_proxy_setting_value(state, account_id)?;
    let Some(bytes) = credentials::load_account_auth_data(&state.crypto, &state.store, account_id)
        .map_err(ApiError::Internal)?
    else {
        return Err(ApiError::BadRequest(format!(
            "no auth data found for account {account_id}"
        )));
    };
    let mut credentials: AccountCredentials = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::Internal(format!("failed to parse stored credentials: {e}")))?;
    set_account_proxy_setting_on_credentials(&mut credentials, setting);
    if current == account_proxy_setting_from_credentials(&credentials) {
        return Ok(());
    }
    let serialized = serde_json::to_vec(&credentials).map_err(ApiError::from_serialize)?;
    credentials::store_account_auth_data(&state.crypto, &state.store, account_id, &serialized)
        .map_err(ApiError::Internal)
}

/// 创建邮件账户（IMAP/SMTP 或 POP3/SMTP）。OAuth 提供器走独立浏览器流程。
pub async fn add_account(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    // 与上游 Tauri 命令签名一致：前端 invoke 传 { request: AddAccountRequest }
    let request: AddAccountRequest =
        serde_json::from_value(args.get("request").cloned().unwrap_or(Value::Null))
            .map_err(|e| ApiError::BadRequest(format!("invalid add_account args: {e}")))?;
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
        let proxy =
            proxy_config_from_parts(request.proxy_host, request.proxy_port, "Account proxy")?;
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
        credentials::store_account_auth_data(
            &state.crypto,
            &state.store,
            &account.id,
            &config_bytes,
        )
        .map_err(|e| PebbleError::Internal(e.to_string()))?;

        state
            .store
            .update_sync_state(&account.id, |s| {
                s.provider = Some(if matches!(provider, ProviderType::Pop3) {
                    "pop3".to_string()
                } else {
                    "imap".to_string()
                });
                if matches!(provider, ProviderType::Imap) {
                    // 新 IMAP 账户默认只同步收件箱，其余邮箱由账户编辑器按需开启
                    s.selected_imap_folder_remote_ids = Some(Vec::new());
                }
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
    let mut creds =
        match credentials::load_account_auth_data(&state.crypto, &state.store, &req.account_id)? {
            Some(bytes) => serde_json::from_slice::<AccountCredentials>(&bytes).map_err(|e| {
                ApiError::Internal(format!("failed to parse stored credentials: {e}"))
            })?,
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
            proxy_config_from_parts(req.proxy_host.clone(), req.proxy_port, "Account proxy")
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

fn account_attachment_paths(
    store: &pebble_store::Store,
    account_id: &str,
) -> Result<Vec<String>, PebbleError> {
    let mut paths = HashSet::new();
    for message_id in store.list_message_ids_by_account(account_id)? {
        for attachment in store.list_attachments_by_message(&message_id)? {
            if let Some(path) = attachment.local_path {
                paths.insert(path);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

/// Remove only files owned by the configured attachment root. Attachment
/// paths are persisted data, so validate the canonical path before deleting
/// anything and leave missing/already-cleaned files alone.
fn cleanup_account_attachment_paths(attachments_dir: &Path, paths: &[String]) {
    let Ok(root) = attachments_dir.canonicalize() else {
        return;
    };
    for raw_path in paths {
        let candidate = Path::new(raw_path);
        let candidate = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
        };
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(&root) || canonical == root || !canonical.is_file() {
            continue;
        }
        let _ = std::fs::remove_file(&canonical);
        let mut parent = canonical.parent().map(PathBuf::from);
        while let Some(dir) = parent {
            if dir == root || !dir.starts_with(&root) {
                break;
            }
            let is_empty = std::fs::read_dir(&dir)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !is_empty || std::fs::remove_dir(&dir).is_err() {
                break;
            }
            parent = dir.parent().map(PathBuf::from);
        }
    }
}

pub async fn delete_account(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let req: DeleteAccountRequest = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid delete_account args: {e}")))?;

    if !state
        .sync_manager
        .stop_account_and_wait(&req.account_id, std::time::Duration::from_secs(30))
        .await
    {
        return Err(ApiError::Internal(format!(
            "timed out waiting for account sync to stop: {}",
            req.account_id
        )));
    }
    let attachment_paths =
        account_attachment_paths(&state.store, &req.account_id).map_err(ApiError::from_store)?;
    state
        .store
        .delete_account(&req.account_id)
        .map_err(ApiError::from_store)?;
    credentials::clear_account_auth_data(&state.store, &req.account_id)?;
    state
        .search
        .delete_by_account(&req.account_id)
        .map_err(ApiError::from_pebble)?;
    cleanup_account_attachment_paths(&state.attachments_dir, &attachment_paths);
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::cleanup_account_attachment_paths;

    #[test]
    fn account_attachment_cleanup_stays_inside_root() {
        let root =
            std::env::temp_dir().join(format!("pw-account-cleanup-{}", uuid::Uuid::new_v4()));
        let owned = root.join("message-1").join("attachment-1");
        let outside = root
            .parent()
            .expect("temp root has a parent")
            .join(format!("pw-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
        std::fs::write(&owned, b"owned").unwrap();
        std::fs::write(&outside, b"outside").unwrap();

        cleanup_account_attachment_paths(
            &root,
            &[
                owned.to_string_lossy().into_owned(),
                outside.to_string_lossy().into_owned(),
            ],
        );

        assert!(!owned.exists());
        assert!(outside.exists());
        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(root);
    }
}
