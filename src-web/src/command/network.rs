use pebble_core::{HttpProxyConfig, PebbleError};
use pebble_mail::ProxyConfig;
use pebble_store::Store;
use serde::Deserialize;
use serde_json::Value;

use crate::command::accounts::AccountProxyMode;
use crate::credentials;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 与桌面端 network.rs 一致的全局代理存储（secure_user_data + 专属 key）。
const GLOBAL_PROXY_KEY: &str = "global_network_proxy";

pub(crate) fn get_global_proxy_raw(
    store: &Store,
    crypto: &pebble_crypto::CryptoService,
) -> Result<Option<HttpProxyConfig>, PebbleError> {
    let Some(encrypted) = store.get_secure_user_data(GLOBAL_PROXY_KEY)? else {
        return Ok(None);
    };
    let plaintext = crypto.decrypt_for(
        credentials::SECURE_USER_DATA_PURPOSE,
        GLOBAL_PROXY_KEY,
        &encrypted,
    )?;
    serde_json::from_slice(&plaintext)
        .map(Some)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse global proxy: {e}")))
}

fn set_global_proxy_raw(
    store: &Store,
    crypto: &pebble_crypto::CryptoService,
    proxy: Option<HttpProxyConfig>,
) -> Result<(), PebbleError> {
    match proxy {
        Some(proxy) => {
            let plaintext = serde_json::to_vec(&proxy)
                .map_err(|e| PebbleError::Internal(format!("Failed to serialize proxy: {e}")))?;
            let encrypted = crypto.encrypt_for(
                credentials::SECURE_USER_DATA_PURPOSE,
                GLOBAL_PROXY_KEY,
                &plaintext,
            )?;
            store.set_secure_user_data(GLOBAL_PROXY_KEY, &encrypted)
        }
        None => store.delete_secure_user_data(GLOBAL_PROXY_KEY),
    }
}

/// 从 auth_data JSON 读取 accounts 的代理模式（默认 Inherit）。
pub(crate) fn account_proxy_mode_from_auth_value(value: &Value) -> AccountProxyMode {
    value
        .get("proxy_mode")
        .and_then(|mode| serde_json::from_value(mode.clone()).ok())
        .unwrap_or_default()
}

/// 依代理模式解析生效代理：Inherit→账户代理或全局代理；Disabled→禁用；Custom→账户代理。
/// 与桌面端 resolve_mail_proxy_from_mode 语义一致。
pub(crate) fn resolve_mail_proxy_from_mode(
    store: &Store,
    crypto: &pebble_crypto::CryptoService,
    mode: AccountProxyMode,
    account_proxy: Option<ProxyConfig>,
) -> Result<Option<ProxyConfig>, PebbleError> {
    let account_proxy = account_proxy.map(|p| HttpProxyConfig {
        host: p.host,
        port: p.port,
    });
    let global_proxy = get_global_proxy_raw(store, crypto)?;
    let effective = match mode {
        AccountProxyMode::Inherit => account_proxy.or(global_proxy),
        AccountProxyMode::Disabled => None,
        AccountProxyMode::Custom => account_proxy,
    };
    Ok(effective.map(|p| ProxyConfig {
        host: p.host,
        port: p.port,
    }))
}

// ─── 命令 ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct UpdateGlobalProxyArgs {
    #[serde(default)]
    pub proxy_host: Option<String>,
    #[serde(default)]
    pub proxy_port: Option<u16>,
}

pub async fn get_global_proxy(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let proxy = get_global_proxy_raw(&state.store, &state.crypto)?;
    serde_json::to_value(proxy).map_err(ApiError::from_serialize)
}

pub async fn update_global_proxy(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: UpdateGlobalProxyArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid update_global_proxy args: {e}")))?;
    let proxy = match (args.proxy_host, args.proxy_port) {
        (Some(host), Some(port)) if !host.trim().is_empty() && port != 0 => Some(HttpProxyConfig {
            host: host.trim().to_string(),
            port,
        }),
        (None, None) => None,
        _ => {
            return Err(ApiError::BadRequest(
                "Global proxy requires both host and port".to_string(),
            ));
        }
    };
    set_global_proxy_raw(&state.store, &state.crypto, proxy)?;
    Ok(Value::Null)
}
