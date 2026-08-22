//! Browser OAuth flow for the Web transport.
//!
//! The authorization code never passes through the frontend.  The server
//! keeps the PKCE verifier and form data in memory, validates the one-time
//! state on callback, exchanges the code, and stores the resulting token blob
//! with the same encrypted `accounts.auth_data` format as the desktop app.

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::Response,
};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    Scope, TokenResponse, TokenUrl,
};
use pebble_core::{new_id, now_timestamp, Account, HttpProxyConfig, PebbleError, ProviderType};
use pebble_oauth::{
    build_http_client, OAuthConfig, OAuthManager, OAuthNetworkConfig, PkceState, TokenPair,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use url::Url;

use crate::account_colors::default_account_color;
use crate::commands::network::AccountProxyMode;
use crate::commands::network;
use crate::blocking::run_blocking;
use crate::commands::encrypted_store;
use crate::error::ApiError;
use crate::state::{AppStateRef, OAuthAccountLockRegistry};

const PENDING_OAUTH_TTL_SECS: i64 = 10 * 60;
const CALLBACK_PATH: &str = "/api/v1/oauth/callback";

async fn oauth_account_lock(
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

fn parse_web_oauth_urls(
    config: &OAuthConfig,
    redirect_url: &str,
) -> Result<(AuthUrl, TokenUrl, RedirectUrl), String> {
    let auth_url = AuthUrl::new(config.auth_url.clone())
        .map_err(|e| format!("Invalid OAuth authorization URL: {e}"))?;
    let token_url = TokenUrl::new(config.token_url.clone())
        .map_err(|e| format!("Invalid OAuth token URL: {e}"))?;
    let redirect_url = RedirectUrl::new(redirect_url.to_string())
        .map_err(|e| format!("Invalid OAuth redirect URL: {e}"))?;
    Ok((auth_url, token_url, redirect_url))
}

/// Build the Web authorization request with the public callback URI. The
/// shared OAuth manager intentionally keeps its desktop localhost redirect;
/// this transport-local helper avoids changing that upstream-facing contract.
fn start_web_oauth(
    config: &OAuthConfig,
    redirect_url: &str,
) -> Result<(String, PkceState), String> {
    let (auth_url, token_url, redirect_url) = parse_web_oauth_urls(config, redirect_url)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let client = oauth2::basic::BasicClient::new(ClientId::new(config.client_id.clone()));
    let client = match config
        .client_secret
        .as_deref()
        .filter(|secret| !secret.is_empty())
    {
        Some(secret) => client.set_client_secret(ClientSecret::new(secret.to_string())),
        None => client,
    }
    .set_auth_uri(auth_url)
    .set_token_uri(token_url)
    .set_redirect_uri(redirect_url);
    let mut request = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(challenge);
    for scope in &config.scopes {
        request = request.add_scope(Scope::new(scope.clone()));
    }
    let (authorization_url, csrf_token) = request.url();
    Ok((
        authorization_url.to_string(),
        PkceState {
            verifier,
            csrf_token,
        },
    ))
}

async fn complete_web_oauth(
    config: &OAuthConfig,
    network: &OAuthNetworkConfig,
    redirect_url: &str,
    code: &str,
    pkce_state: PkceState,
) -> Result<TokenPair, String> {
    let (auth_url, token_url, redirect_url) = parse_web_oauth_urls(config, redirect_url)?;
    let client = oauth2::basic::BasicClient::new(ClientId::new(config.client_id.clone()));
    let client = match config
        .client_secret
        .as_deref()
        .filter(|secret| !secret.is_empty())
    {
        Some(secret) => client.set_client_secret(ClientSecret::new(secret.to_string())),
        None => client,
    }
    .set_auth_uri(auth_url)
    .set_token_uri(token_url)
    .set_redirect_uri(redirect_url);
    let http_client = build_http_client(network).map_err(|e| e.to_string())?;
    let response = client
        .exchange_code(AuthorizationCode::new(code.to_string()))
        .set_pkce_verifier(pkce_state.verifier)
        .request_async(&http_client)
        .await
        .map_err(|e| e.to_string())?;
    Ok(token_response_to_pair(&response, None))
}

fn token_response_to_pair(
    response: &oauth2::basic::BasicTokenResponse,
    fallback_refresh: Option<&str>,
) -> TokenPair {
    let expires_at = response.expires_in().map(|duration| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            + duration.as_secs() as i64
    });
    let scopes = response
        .scopes()
        .map(|scopes| {
            scopes
                .iter()
                .map(|scope| scope.as_ref().to_string())
                .collect()
        })
        .unwrap_or_default();
    let refresh_token = response
        .refresh_token()
        .map(|token| token.secret().clone())
        .or_else(|| fallback_refresh.map(str::to_string));
    TokenPair {
        access_token: response.access_token().secret().clone(),
        refresh_token,
        expires_at,
        scopes,
    }
}

/// A pending transaction is deliberately server-side and non-serializable.
/// `csrf_token.secret()` is the map key, so the callback can atomically remove
/// it before doing any network or storage work (one-time use).
pub(crate) struct PendingOAuth {
    pub(crate) provider: String,
    pub(crate) email: String,
    pub(crate) display_name: String,
    pub(crate) account_proxy: Option<HttpProxyConfig>,
    pub(crate) redirect_url: String,
    pub(crate) config: OAuthConfig,
    pub(crate) network: OAuthNetworkConfig,
    pub(crate) pkce_state: Option<PkceState>,
    pub(crate) created_at: i64,
}

/// Decrypted OAuth material used only while constructing a mail provider.
/// The access/refresh tokens are never serialized into a response.
pub(crate) struct OAuthAccess {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) expires_at: Option<i64>,
    pub(crate) proxy: Option<HttpProxyConfig>,
}

pub(crate) fn load_oauth_access(
    crypto: &pebble_crypto::CryptoService,
    store: &pebble_store::Store,
    account_id: &str,
) -> Result<OAuthAccess, PebbleError> {
    let bytes = encrypted_store::load_account_auth_data(crypto, store, account_id)?
        .ok_or_else(|| PebbleError::Auth(format!("No OAuth auth data for account {account_id}")))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| PebbleError::Auth(format!("Invalid OAuth auth data: {e}")))?;
    let access_token = value["access_token"]
        .as_str()
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| PebbleError::Auth("OAuth access token is missing".to_string()))?
        .to_string();
    let refresh_token = value["refresh_token"].as_str().map(ToOwned::to_owned);
    let expires_at = value["expires_at"].as_i64();
    let stored_proxy = value
        .get("proxy")
        .filter(|proxy| !proxy.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| PebbleError::Auth(format!("Invalid OAuth proxy config: {e}")))?;
    let mode = value
        .get("proxy_mode")
        .and_then(|mode| serde_json::from_value(mode.clone()).ok())
        .unwrap_or(AccountProxyMode::Inherit);
    let proxy = match mode {
        AccountProxyMode::Disabled => None,
        AccountProxyMode::Custom => stored_proxy,
        AccountProxyMode::Inherit => stored_proxy.or(network::get_global_proxy_raw(crypto, store)?),
    };
    Ok(OAuthAccess {
        access_token,
        refresh_token,
        expires_at,
        proxy,
    })
}

/// Construct a shared Gmail/Outlook provider with the account's effective
/// proxy and a refreshed access token when the stored token is near expiry.
/// Keeping this adapter in `src-web` avoids adding Web transport concerns to
/// the shared OAuth crate.
pub(crate) async fn load_oauth_provider(
    state: &AppStateRef,
    account: &Account,
) -> Result<Arc<dyn pebble_core::traits::MailProvider>, PebbleError> {
    let access = load_oauth_access(&state.crypto, &state.store, &account.id)?;
    let provider_name = match account.provider {
        ProviderType::Gmail => "gmail",
        ProviderType::Outlook => "outlook",
        _ => {
            return Err(PebbleError::UnsupportedProvider(
                "OAuth provider is required".to_string(),
            ))
        }
    };

    let mut access_token = access.access_token.clone();
    if access
        .expires_at
        .is_some_and(|expires_at| expires_at <= now_timestamp() + 60)
    {
        if let Some(refresher) = build_oauth_token_refresher(
            state.crypto.clone(),
            state.store.clone(),
            provider_name,
            &access,
            state.oauth_account_locks.clone(),
            &account.id,
        )? {
            let (refreshed, _) = refresher().await?;
            access_token = refreshed;
        }
    }

    let credentials = match access.proxy {
        Some(proxy) => json!({
            "access_token": access_token,
            "proxy": proxy,
        }),
        None => json!({"access_token": access_token}),
    };
    pebble_mail::provider::create_provider(&account.provider, &credentials, &account.id).await
}

/// Build the refresh closure expected by Gmail/Outlook sync workers. The
/// closure re-reads the encrypted JSON and updates only token fields, so a
/// concurrent proxy preference update is not overwritten.
pub(crate) fn build_oauth_token_refresher(
    crypto: Arc<pebble_crypto::CryptoService>,
    store: Arc<pebble_store::Store>,
    provider: &str,
    access: &OAuthAccess,
    account_locks: OAuthAccountLockRegistry,
    account_id: &str,
) -> Result<Option<pebble_mail::gmail_sync::TokenRefresher>, PebbleError> {
    let Some(initial_refresh_token) = access.refresh_token.clone() else {
        return Ok(None);
    };
    let config = oauth_config_for_provider(provider).map_err(PebbleError::Auth)?;
    let account_id = account_id.to_string();
    Ok(Some(Box::new(move || {
        let config = config.clone();
        let initial_refresh_token = initial_refresh_token.clone();
        let crypto = crypto.clone();
        let store = store.clone();
        let account_locks = account_locks.clone();
        let account_id = account_id.clone();
        Box::pin(async move {
            let account_lock = oauth_account_lock(&account_locks, &account_id).await;
            let _account_guard = account_lock.lock().await;

            // Re-read the latest encrypted auth blob after taking the per-account lock.
            // Providers can rotate refresh tokens, so a token captured before another
            // refresh completed must not overwrite the newer value.
            let existing = encrypted_store::load_account_auth_data(&crypto, &store, &account_id)?
                .ok_or_else(|| {
                    PebbleError::Auth("OAuth auth data disappeared during refresh".to_string())
                })?;
            let mut value: Value = serde_json::from_slice(&existing)
                .map_err(|e| PebbleError::Auth(format!("Invalid OAuth auth data: {e}")))?;
            let refresh_token = value["refresh_token"]
                .as_str()
                .map(ToOwned::to_owned)
                .unwrap_or(initial_refresh_token);

            let current_access = load_oauth_access(&crypto, &store, &account_id)?;
            let manager = OAuthManager::new_with_network(
                config,
                OAuthNetworkConfig {
                    proxy: current_access.proxy,
                },
            );
            let token_pair = manager
                .refresh_token(&refresh_token)
                .await
                .map_err(|e| PebbleError::Auth(format!("OAuth token refresh failed: {e}")))?;

            value["access_token"] = Value::String(token_pair.access_token.clone());
            value["refresh_token"] = token_pair
                .refresh_token
                .clone()
                .map(Value::String)
                .unwrap_or_else(|| Value::String(refresh_token));
            value["expires_at"] = token_pair
                .expires_at
                .map(Value::from)
                .unwrap_or(Value::Null);
            value["scopes"] = serde_json::to_value(&token_pair.scopes)
                .map_err(|e| PebbleError::Internal(e.to_string()))?;
            let bytes =
                serde_json::to_vec(&value).map_err(|e| PebbleError::Internal(e.to_string()))?;
            encrypted_store::store_account_auth_data(&crypto, &store, &account_id, &bytes)?;
            Ok((token_pair.access_token, token_pair.expires_at))
        })
    })))
}

#[derive(Deserialize)]
struct StartOAuthArgs {
    provider: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    proxy_host: Option<String>,
    #[serde(default)]
    proxy_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OAuthCallbackQuery {
    pub(crate) state: Option<String>,
    pub(crate) code: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) error_description: Option<String>,
}

/// Begin an OAuth transaction. The frontend opens the returned URL in a
/// popup; the callback later posts the created account back to that opener.
pub(crate) async fn start_oauth_flow(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: StartOAuthArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid complete_oauth_flow args: {e}")))?;
    let provider = normalize_provider(&args.provider).map_err(ApiError::BadRequest)?;
    let redirect_url = oauth_redirect_url().map_err(ApiError::BadRequest)?;
    let config = oauth_config_for_provider(&provider).map_err(ApiError::BadRequest)?;
    let account_proxy = proxy_from_parts(args.proxy_host, args.proxy_port)?;
    let effective_proxy = match account_proxy.clone() {
        Some(proxy) => Some(proxy),
        None => network::get_global_proxy_raw(&state.crypto, &state.store)
            .map_err(ApiError::from_pebble)?,
    };
    let network = OAuthNetworkConfig {
        proxy: effective_proxy,
    };
    let (authorization_url, pkce_state) = start_web_oauth(&config, &redirect_url)
        .map_err(|e| ApiError::BadRequest(format!("failed to start {provider} OAuth: {e}")))?;
    let state_key = pkce_state.csrf_token.secret().to_string();

    let mut pending = state.oauth_pending.lock().await;
    let now = now_timestamp();
    pending.retain(|_, item| item.created_at + PENDING_OAUTH_TTL_SECS > now);
    pending.insert(
        state_key,
        PendingOAuth {
            provider,
            email: args.email,
            display_name: args.display_name,
            account_proxy,
            redirect_url,
            config,
            network,
            pkce_state: Some(pkce_state),
            created_at: now,
        },
    );

    Ok(json!({ "authorization_url": authorization_url }))
}

/// OAuth provider callback. It intentionally does not require the normal JWT:
/// the one-time server-side OAuth state is the callback credential.
pub(crate) async fn oauth_callback(
    State(state): State<AppStateRef>,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    let Some(state_key) = query.state.as_deref().filter(|value| !value.is_empty()) else {
        return callback_page_error("OAuth callback is missing state");
    };
    let pending = state.oauth_pending.lock().await.remove(state_key);
    let Some(mut pending) = pending else {
        return callback_page_error("OAuth state is invalid, expired, or already used");
    };
    if pending.created_at + PENDING_OAUTH_TTL_SECS <= now_timestamp()
        || pending
            .pkce_state
            .as_ref()
            .is_none_or(|pkce_state| !constant_time_eq(state_key, pkce_state.csrf_token.secret()))
    {
        return callback_page_error("OAuth state is invalid or expired");
    }
    if let Some(error) = query.error.as_deref() {
        let detail = query
            .error_description
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|description| format!(" ({description})"))
            .unwrap_or_default();
        return callback_page_error(&format!("OAuth authorization was denied: {error}{detail}"));
    }
    let Some(code) = query.code.as_deref().filter(|value| !value.is_empty()) else {
        return callback_page_error("OAuth callback is missing authorization code");
    };

    let Some(pkce_state) = pending.pkce_state.take() else {
        return callback_page_error("OAuth state is invalid or already used");
    };
    let tokens = match complete_web_oauth(
        &pending.config,
        &pending.network,
        &pending.redirect_url,
        code,
        pkce_state,
    )
    .await
    {
        Ok(tokens) => tokens,
        Err(error) => return callback_page_error(&format!("OAuth token exchange failed: {error}")),
    };
    let identity =
        match fetch_userinfo(&pending.provider, &tokens.access_token, &pending.network).await {
            Ok(identity) => identity,
            Err(_error) if pending.provider == "gmail" => {
                (pending.email.clone(), pending.display_name.clone())
            }
            Err(error) => return callback_page_error(&error),
        };

    let account = match persist_oauth_account(&state, &pending, &tokens, identity).await {
        Ok(account) => account,
        Err(error) => return callback_page_error(&error),
    };
    let sync = state.sync_manager.clone();
    let account_for_sync = account.clone();
    tokio::spawn(async move {
        if let Err(error) = sync.sync_account(&account_for_sync).await {
            tracing::warn!(account_id = %account_for_sync.id, "initial OAuth sync failed: {error}");
        }
    });
    callback_page_success(&account)
}

async fn persist_oauth_account(
    state: &AppStateRef,
    pending: &PendingOAuth,
    tokens: &TokenPair,
    identity: (String, String),
) -> Result<Account, String> {
    let provider = pending.provider.clone();
    let email = if identity.0.trim().is_empty() {
        pending.email.clone()
    } else {
        identity.0
    };
    let display_name = if identity.1.trim().is_empty() {
        pending.display_name.clone()
    } else {
        identity.1
    };
    let account_proxy = pending.account_proxy.clone();
    let token_json = json!({
        "access_token": tokens.access_token,
        "refresh_token": tokens.refresh_token,
        "expires_at": tokens.expires_at,
        "scopes": tokens.scopes,
        "proxy_mode": if account_proxy.is_some() { "custom" } else { "inherit" },
        "proxy": account_proxy,
    });
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let provider_type = provider_type(&provider).map_err(|e| e.to_string())?;
    run_blocking(move || {
        let accounts = store.list_accounts()?;
        let now = now_timestamp();
        let account = Account {
            id: new_id(),
            email,
            display_name,
            color: Some(default_account_color(&accounts, &provider)),
            provider: provider_type,
            created_at: now,
            updated_at: now,
        };
        store.insert_account(&account)?;
        let result = (|| -> Result<(), PebbleError> {
            let bytes = serde_json::to_vec(&token_json).map_err(|e| {
                PebbleError::Internal(format!("failed to serialize OAuth auth data: {e}"))
            })?;
            encrypted_store::store_account_auth_data(&crypto, &store, &account.id, &bytes)?;
            store.update_sync_state(&account.id, |sync_state| {
                sync_state.provider = Some(provider.clone());
            })?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = store.delete_account(&account.id);
            return Err(error);
        }
        Ok(account)
    })
    .await
    .map_err(|error| format!("OAuth account persistence failed: {error:?}"))
}

async fn fetch_userinfo(
    provider: &str,
    access_token: &str,
    network: &OAuthNetworkConfig,
) -> Result<(String, String), String> {
    let url = match provider {
        "gmail" => "https://www.googleapis.com/oauth2/v2/userinfo",
        "outlook" => {
            "https://graph.microsoft.com/v1.0/me?$select=mail,userPrincipalName,displayName"
        }
        _ => return Err(format!("unsupported OAuth provider: {provider}")),
    };
    let client = build_http_client(network).map_err(|e| e.to_string())?;
    let value: Value = client
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("OAuth user profile request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("OAuth user profile request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("OAuth user profile response was invalid: {e}"))?;
    let email = if provider == "outlook" {
        value["mail"]
            .as_str()
            .or_else(|| value["userPrincipalName"].as_str())
    } else {
        value["email"].as_str()
    }
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .ok_or_else(|| "OAuth user profile did not include a mailbox address".to_string())?;
    let name = value["displayName"]
        .as_str()
        .or_else(|| value["name"].as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok((email.to_string(), name))
}

fn normalize_provider(provider: &str) -> Result<String, String> {
    let provider = provider.trim().to_ascii_lowercase();
    if matches!(provider.as_str(), "gmail" | "outlook") {
        Ok(provider)
    } else {
        Err(format!("unsupported OAuth provider: {provider}"))
    }
}

fn provider_type(provider: &str) -> Result<ProviderType, PebbleError> {
    match provider {
        "gmail" => Ok(ProviderType::Gmail),
        "outlook" => Ok(ProviderType::Outlook),
        _ => Err(PebbleError::UnsupportedProvider(provider.to_string())),
    }
}

fn oauth_config_for_provider(provider: &str) -> Result<OAuthConfig, String> {
    let (client_id_key, client_secret_key, auth_url, token_url, scopes) = match provider {
        "gmail" => (
            "GOOGLE_CLIENT_ID",
            "GOOGLE_CLIENT_SECRET",
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            vec![
                "https://mail.google.com/".to_string(),
                "https://www.googleapis.com/auth/userinfo.email".to_string(),
                "https://www.googleapis.com/auth/userinfo.profile".to_string(),
            ],
        ),
        "outlook" => (
            "MICROSOFT_CLIENT_ID",
            "MICROSOFT_CLIENT_SECRET",
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            vec![
                "https://graph.microsoft.com/Mail.ReadWrite".to_string(),
                "https://graph.microsoft.com/Mail.Send".to_string(),
                "https://graph.microsoft.com/User.Read".to_string(),
                "offline_access".to_string(),
            ],
        ),
        _ => return Err(format!("unsupported OAuth provider: {provider}")),
    };
    let client_id = std::env::var(client_id_key).unwrap_or_default();
    if is_placeholder(&client_id) {
        return Err(format!("{client_id_key} is not configured"));
    }
    let client_secret = std::env::var(client_secret_key)
        .ok()
        .filter(|value| !is_placeholder(value));
    Ok(OAuthConfig {
        client_id,
        client_secret,
        auth_url: auth_url.to_string(),
        token_url: token_url.to_string(),
        scopes,
        redirect_port: 0,
    })
}

fn oauth_redirect_url() -> Result<String, String> {
    let value = std::env::var("PEBBLE_OAUTH_REDIRECT_URL")
        .map_err(|_| "PEBBLE_OAUTH_REDIRECT_URL is required for Web OAuth".to_string())?;
    let url = Url::parse(&value).map_err(|e| format!("invalid PEBBLE_OAUTH_REDIRECT_URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("PEBBLE_OAUTH_REDIRECT_URL must be an absolute HTTP(S) URL".to_string());
    }
    if url.path() != CALLBACK_PATH {
        return Err(format!(
            "PEBBLE_OAUTH_REDIRECT_URL must end with {CALLBACK_PATH}"
        ));
    }
    Ok(value)
}

fn proxy_from_parts(
    host: Option<String>,
    port: Option<u16>,
) -> Result<Option<HttpProxyConfig>, ApiError> {
    match (host, port) {
        (None, None) => Ok(None),
        (Some(host), None) if host.trim().is_empty() => Ok(None),
        (Some(host), Some(port)) => {
            let proxy = HttpProxyConfig {
                host: host.trim().to_string(),
                port,
            };
            proxy.validate().map_err(ApiError::BadRequest)?;
            Ok(Some(proxy))
        }
        (Some(_), None) => Err(ApiError::BadRequest(
            "OAuth proxy port is required when proxy host is set".to_string(),
        )),
        (None, Some(_)) => Err(ApiError::BadRequest(
            "OAuth proxy host is required when proxy port is set".to_string(),
        )),
    }
}

fn is_placeholder(value: &str) -> bool {
    let value = value.trim();
    value.is_empty()
        || value.eq_ignore_ascii_case("YOUR_CLIENT_ID")
        || value.eq_ignore_ascii_case("YOUR_CLIENT_SECRET")
        || value.ends_with("_PLACEHOLDER")
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn callback_page_success(account: &Account) -> Response {
    callback_page(json!({ "status": "success", "account": account }))
}

fn callback_page_error(message: &str) -> Response {
    callback_page(json!({ "status": "error", "message": message }))
}

fn callback_page(payload: Value) -> Response {
    // Escape `<` before embedding JSON in a script element. The opener still
    // receives the original JSON value after JavaScript parses it.
    let payload = serde_json::to_string(&payload)
        .unwrap_or_else(|_| r#"{"status":"error","message":"OAuth callback failed"}"#.to_string())
        .replace('<', "\\u003c");
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Pebble OAuth</title>\
         <p id=\"message\">Completing sign-in…</p>\
         <script>const payload={payload};\
         if(window.opener&&!window.opener.closed){{window.opener.postMessage({{type:'pebble-oauth',...payload}},window.location.origin);}}\
         document.getElementById('message').textContent=payload.status==='success'?'Sign-in complete. You can close this window.':(payload.message||'Sign-in failed.');\
         if(payload.status==='success'){{setTimeout(()=>window.close(),250);}}</script>",
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(axum::body::Body::from(html))
        .expect("OAuth callback response should be valid")
}
