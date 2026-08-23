use pebble_core::{
    now_timestamp, HttpProxyConfig, OAuthTokens, PebbleError, ProviderType,
};
use pebble_crypto::CryptoService;
use pebble_mail::gmail_sync::TokenRefresher;
use pebble_oauth::{OAuthConfig, OAuthManager, OAuthNetworkConfig};
use pebble_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::blocking::run_blocking;
use crate::commands::encrypted_store::{load_account_auth_data, store_account_auth_data};
use crate::commands::network::{
    account_proxy_setting_from_parts, get_global_proxy_raw, normalize_account_proxy_setting,
    proxy_config_from_parts, resolve_effective_proxy_setting, AccountProxyMode, AccountProxySetting,
};
use crate::error::ApiError;
use crate::state::{AppState, AppStateRef, OAuthAccountLockRegistry};

#[derive(Deserialize)]
struct AccountProxyArgs {
    account_id: String,
}

#[derive(Deserialize)]
struct UpdateOAuthProxyArgs {
    account_id: String,
    #[serde(default)]
    proxy_host: Option<String>,
    #[serde(default)]
    proxy_port: Option<u16>,
}

#[derive(Deserialize)]
struct UpdateOAuthProxySettingArgs {
    account_id: String,
    mode: AccountProxyMode,
    #[serde(default)]
    proxy_host: Option<String>,
    #[serde(default)]
    proxy_port: Option<u16>,
}

pub(crate) async fn oauth_account_lock(
    registry: &OAuthAccountLockRegistry,
    account_id: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = registry.lock().await;
    Arc::clone(
        locks
            .entry(account_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
}

fn provider_slug(provider: &ProviderType) -> &'static str {
    match provider {
        ProviderType::Imap => "imap",
        ProviderType::Pop3 => "pop3",
        ProviderType::Gmail => "gmail",
        ProviderType::Outlook => "outlook",
    }
}

fn ensure_oauth_account_provider(
    state: &AppState,
    account_id: &str,
) -> Result<(), PebbleError> {
    let account = state
        .store
        .get_account(account_id)?
        .ok_or_else(|| PebbleError::Internal(format!("Account not found: {account_id}")))?;
    if matches!(account.provider, ProviderType::Gmail | ProviderType::Outlook) {
        Ok(())
    } else {
        Err(PebbleError::UnsupportedProvider(
            provider_slug(&account.provider).to_string(),
        ))
    }
}

pub(crate) fn gmail_oauth_config() -> OAuthConfig {
    crate::oauth::gmail_oauth_config()
}

pub(crate) fn outlook_oauth_config() -> OAuthConfig {
    crate::oauth::outlook_oauth_config()
}

fn is_placeholder(value: &str) -> bool {
    let v = value.trim();
    v.is_empty()
        || v.eq_ignore_ascii_case("YOUR_CLIENT_ID")
        || v.eq_ignore_ascii_case("YOUR_CLIENT_SECRET")
        || v.ends_with("_PLACEHOLDER")
}

fn validate_oauth_config(config: &OAuthConfig, provider: &str) -> Result<(), PebbleError> {
    if is_placeholder(&config.client_id) {
        return Err(PebbleError::Internal(format!(
            "OAuth client_id for '{provider}' is not configured. Set the appropriate environment variable before starting the OAuth flow."
        )));
    }
    if let Some(secret) = &config.client_secret {
        if is_placeholder(secret) {
            return Err(PebbleError::Internal(format!(
                "OAuth client_secret for '{provider}' is not configured. Set the appropriate environment variable before starting the OAuth flow."
            )));
        }
    }
    Ok(())
}

pub(crate) fn config_for_provider(provider: &str) -> Result<OAuthConfig, PebbleError> {
    let config = match provider.to_lowercase().as_str() {
        "gmail" => gmail_oauth_config(),
        "outlook" => outlook_oauth_config(),
        _ => {
            return Err(PebbleError::UnsupportedProvider(format!(
                "Unknown OAuth provider: {provider}"
            )))
        }
    };
    validate_oauth_config(&config, provider)?;
    Ok(config)
}

fn persist_oauth_tokens(
    state: &AppState,
    account_id: &str,
    tokens: &OAuthTokens,
) -> Result<(), PebbleError> {
    persist_oauth_tokens_raw(&state.crypto, &state.store, account_id, tokens)
}

fn persist_oauth_tokens_raw(
    crypto: &CryptoService,
    store: &Store,
    account_id: &str,
    tokens: &OAuthTokens,
) -> Result<(), PebbleError> {
    let (proxy_mode, proxy) = read_stored_oauth_auth_data_raw(crypto, store, account_id)?
        .map(|stored| (stored.proxy_mode, stored.proxy))
        .unwrap_or((AccountProxyMode::Inherit, None));
    let stored =
        StoredOAuthAuthData::from_tokens_with_proxy_mode(tokens.clone(), proxy_mode, proxy);
    persist_stored_oauth_auth_data_raw(crypto, store, account_id, &stored)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredOAuthAuthData {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "crate::commands::network::is_inherit_proxy_mode")]
    proxy_mode: AccountProxyMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proxy: Option<HttpProxyConfig>,
}

impl StoredOAuthAuthData {
    pub(crate) fn from_tokens(tokens: OAuthTokens, proxy: Option<HttpProxyConfig>) -> Self {
        let proxy_mode = if proxy.is_some() {
            AccountProxyMode::Custom
        } else {
            AccountProxyMode::Inherit
        };
        Self::from_tokens_with_proxy_mode(tokens, proxy_mode, proxy)
    }

    fn from_tokens_with_proxy_mode(
        tokens: OAuthTokens,
        proxy_mode: AccountProxyMode,
        proxy: Option<HttpProxyConfig>,
    ) -> Self {
        Self {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: tokens.expires_at,
            scopes: tokens.scopes,
            proxy_mode,
            proxy,
        }
    }

    fn with_proxy_setting(mut self, setting: AccountProxySetting) -> Self {
        self.proxy_mode = setting.mode;
        self.proxy = setting.proxy;
        self
    }

    fn tokens(&self) -> OAuthTokens {
        OAuthTokens {
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at: self.expires_at,
            scopes: self.scopes.clone(),
        }
    }
}

fn decode_stored_oauth_auth_data(bytes: &[u8]) -> Result<StoredOAuthAuthData, PebbleError> {
    serde_json::from_slice(bytes)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse OAuth auth data: {e}")))
}

pub(crate) fn persist_stored_oauth_auth_data_raw(
    crypto: &CryptoService,
    store: &Store,
    account_id: &str,
    stored: &StoredOAuthAuthData,
) -> Result<(), PebbleError> {
    let config_bytes = serde_json::to_vec(stored)
        .map_err(|e| PebbleError::Internal(format!("Failed to serialize OAuth auth data: {e}")))?;
    store_account_auth_data(crypto, store, account_id, &config_bytes)
}

fn read_stored_oauth_auth_data_raw(
    crypto: &CryptoService,
    store: &Store,
    account_id: &str,
) -> Result<Option<StoredOAuthAuthData>, PebbleError> {
    let Some(decrypted) = load_account_auth_data(crypto, store, account_id)? else {
        return Ok(None);
    };
    decode_stored_oauth_auth_data(&decrypted).map(Some)
}

fn effective_oauth_proxy(
    crypto: &CryptoService,
    store: &Store,
    stored: &StoredOAuthAuthData,
) -> Result<Option<HttpProxyConfig>, PebbleError> {
    let global_proxy = get_global_proxy_raw(crypto, store)?;
    Ok(resolve_effective_proxy_setting(
        stored.proxy_mode,
        stored.proxy.clone(),
        global_proxy,
    ))
}

pub(crate) struct DecodedOAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub proxy: Option<HttpProxyConfig>,
}

pub(crate) struct ResolvedOAuthAuth {
    pub tokens: OAuthTokens,
    pub proxy: Option<HttpProxyConfig>,
}

pub(crate) fn decode_oauth_account_tokens_raw(
    crypto: &CryptoService,
    store: &Store,
    account_id: &str,
) -> Result<DecodedOAuthTokens, PebbleError> {
    let stored = read_stored_oauth_auth_data_raw(crypto, store, account_id)?
        .ok_or_else(|| PebbleError::Internal(format!("No auth data for account {account_id}")))?;
    let proxy = effective_oauth_proxy(crypto, store, &stored)?;
    Ok(DecodedOAuthTokens {
        access_token: stored.access_token,
        refresh_token: stored.refresh_token,
        expires_at: stored.expires_at,
        proxy,
    })
}

pub(crate) fn build_oauth_token_refresher(
    oauth_config: OAuthConfig,
    refresh_token: Option<String>,
    fallback_access_token: String,
    crypto: Arc<CryptoService>,
    store: Arc<Store>,
    account_locks: OAuthAccountLockRegistry,
    account_id: String,
) -> TokenRefresher {
    match refresh_token {
        Some(initial_rt) => Box::new(move || {
            let config = oauth_config.clone();
            let crypto = Arc::clone(&crypto);
            let store = Arc::clone(&store);
            let account_locks = Arc::clone(&account_locks);
            let account_id = account_id.clone();
            let initial_rt = initial_rt.clone();
            Box::pin(async move {
                let account_lock = oauth_account_lock(&account_locks, &account_id).await;
                let _account_guard = account_lock.lock().await;
                let (rt, network) = match load_account_auth_data(&crypto, &store, &account_id)? {
                    Some(decrypted) => {
                        let stored = decode_stored_oauth_auth_data(&decrypted)?;
                        let effective_proxy = effective_oauth_proxy(&crypto, &store, &stored)?;
                        (
                            stored.refresh_token.clone().unwrap_or(initial_rt),
                            OAuthNetworkConfig {
                                proxy: effective_proxy,
                            },
                        )
                    }
                    None => {
                        let effective_proxy = get_global_proxy_raw(&crypto, &store)?;
                        (
                            initial_rt,
                            OAuthNetworkConfig {
                                proxy: effective_proxy,
                            },
                        )
                    }
                };

                let manager = OAuthManager::new_with_network(config, network);
                let token_pair = manager
                    .refresh_token(&rt)
                    .await
                    .map_err(|e| PebbleError::OAuth(format!("Token refresh failed: {e}")))?;
                let tokens = OAuthTokens {
                    access_token: token_pair.access_token.clone(),
                    refresh_token: token_pair.refresh_token.clone().or(Some(rt)),
                    expires_at: token_pair.expires_at,
                    scopes: token_pair.scopes.clone(),
                };
                persist_oauth_tokens_raw(&crypto, &store, &account_id, &tokens)?;
                Ok((token_pair.access_token, token_pair.expires_at))
            })
        }),
        None => Box::new(move || {
            let token = fallback_access_token.clone();
            Box::pin(async move { Ok((token, None)) })
        }),
    }
}

pub(crate) async fn ensure_account_oauth_auth(
    state: &AppState,
    account_id: &str,
    provider: &str,
) -> Result<ResolvedOAuthAuth, PebbleError> {
    let account_lock = oauth_account_lock(&state.oauth_account_locks, account_id).await;
    let _account_guard = account_lock.lock().await;
    let stored = read_stored_oauth_auth_data_raw(&state.crypto, &state.store, account_id)?
        .ok_or_else(|| {
            PebbleError::Internal(format!("No auth data found for account {account_id}"))
        })?;
    let proxy = effective_oauth_proxy(&state.crypto, &state.store, &stored)?;
    let network = OAuthNetworkConfig {
        proxy: proxy.clone(),
    };
    let mut tokens = stored.tokens();

    let needs_refresh = tokens.refresh_token.is_some()
        && tokens
            .expires_at
            .map(|exp| exp - now_timestamp() < 300)
            .unwrap_or(false);

    if needs_refresh {
        let refresh_token = tokens.refresh_token.clone().unwrap_or_default();
        let manager = OAuthManager::new_with_network(config_for_provider(provider)?, network);
        let token_pair = manager
            .refresh_token(&refresh_token)
            .await
            .map_err(|e| PebbleError::OAuth(format!("Token refresh failed: {e}")))?;

        tokens = OAuthTokens {
            access_token: token_pair.access_token,
            refresh_token: token_pair.refresh_token.or(Some(refresh_token)),
            expires_at: token_pair.expires_at,
            scopes: token_pair.scopes,
        };
        persist_oauth_tokens(state, account_id, &tokens)?;
    }

    Ok(ResolvedOAuthAuth { tokens, proxy })
}

fn get_oauth_account_proxy_setting_raw(
    state: &AppState,
    account_id: &str,
) -> Result<AccountProxySetting, PebbleError> {
    ensure_oauth_account_provider(state, account_id)?;
    let stored = read_stored_oauth_auth_data_raw(&state.crypto, &state.store, account_id)?
        .ok_or_else(|| {
            PebbleError::Internal(format!("No auth data found for account {account_id}"))
        })?;
    Ok(normalize_account_proxy_setting(
        stored.proxy_mode,
        stored.proxy,
    ))
}

fn update_oauth_account_proxy_setting_raw(
    state: &AppState,
    account_id: &str,
    mode: AccountProxyMode,
    proxy_host: Option<String>,
    proxy_port: Option<u16>,
) -> Result<(), PebbleError> {
    ensure_oauth_account_provider(state, account_id)?;
    let setting =
        account_proxy_setting_from_parts(mode, proxy_host, proxy_port, "OAuth proxy")?;
    let stored = read_stored_oauth_auth_data_raw(&state.crypto, &state.store, account_id)?
        .ok_or_else(|| {
            PebbleError::Internal(format!("No auth data found for account {account_id}"))
        })?
        .with_proxy_setting(setting);
    persist_stored_oauth_auth_data_raw(&state.crypto, &state.store, account_id, &stored)
}

pub async fn get_oauth_account_proxy(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: AccountProxyArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_oauth_account_proxy args: {e}")))?;
    let account_id = args.account_id;
    let setting = run_blocking(move || get_oauth_account_proxy_setting_raw(&state, &account_id)).await?;
    serde_json::to_value(setting.proxy).map_err(ApiError::from_serialize)
}

pub async fn get_oauth_account_proxy_setting(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    let args: AccountProxyArgs = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid get_oauth_account_proxy_setting args: {e}"))
    })?;
    let account_id = args.account_id;
    let setting = run_blocking(move || get_oauth_account_proxy_setting_raw(&state, &account_id)).await?;
    serde_json::to_value(setting).map_err(ApiError::from_serialize)
}

pub async fn update_oauth_account_proxy(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    let args: UpdateOAuthProxyArgs = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid update_oauth_account_proxy args: {e}"))
    })?;
    let proxy = proxy_config_from_parts(args.proxy_host, args.proxy_port, "OAuth proxy")
        .map_err(ApiError::from_pebble)?;
    let (mode, proxy_host, proxy_port) = match proxy {
        Some(proxy) => (
            AccountProxyMode::Custom,
            Some(proxy.host),
            Some(proxy.port),
        ),
        None => (AccountProxyMode::Inherit, None, None),
    };
    update_oauth_account_proxy_setting_value(
        state,
        args.account_id,
        mode,
        proxy_host,
        proxy_port,
    )
    .await?;
    Ok(Value::Null)
}

pub async fn update_oauth_account_proxy_setting(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    let args: UpdateOAuthProxySettingArgs = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!(
            "invalid update_oauth_account_proxy_setting args: {e}"
        ))
    })?;
    update_oauth_account_proxy_setting_value(
        state,
        args.account_id,
        args.mode,
        args.proxy_host,
        args.proxy_port,
    )
    .await?;
    Ok(Value::Null)
}

async fn update_oauth_account_proxy_setting_value(
    state: AppStateRef,
    account_id: String,
    mode: AccountProxyMode,
    proxy_host: Option<String>,
    proxy_port: Option<u16>,
) -> Result<(), ApiError> {
    let account_lock =
        oauth_account_lock(&state.oauth_account_locks, &account_id).await;
    let _account_guard = account_lock.lock().await;
    run_blocking(move || {
        update_oauth_account_proxy_setting_raw(
            &state,
            &account_id,
            mode,
            proxy_host,
            proxy_port,
        )
    })
    .await
}
