//! Browser OAuth flow for the Web transport.
//!
//! The authorization code never passes through the frontend.  The server
//! keeps the PKCE verifier and form data in memory, validates the one-time
//! state on callback, exchanges the code, and stores the resulting token blob
//! with the same encrypted `accounts.auth_data` format as the desktop app.

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    Json,
};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    Scope, TokenResponse, TokenUrl,
};
use pebble_core::{
    new_id, now_timestamp, Account, HttpProxyConfig, OAuthTokens, PebbleError, ProviderType,
};
use pebble_oauth::{
    build_http_client, OAuthConfig, OAuthNetworkConfig, PkceState, TokenPair,
};
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

use crate::account_colors::default_account_color;
use crate::commands::network;
use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

const PENDING_OAUTH_TTL_SECS: i64 = 5 * 60;
const OAUTH_TERMINAL_STATUS_RETENTION_SECS: i64 = 10 * 60;
const CALLBACK_PATH: &str = "/api/v1/oauth/callback";

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

fn token_exchange_error_message(provider: &str, detail: &str) -> String {
    let detail_lower = detail.to_ascii_lowercase();
    if detail_lower.contains("client_secret is missing") {
        if provider.eq_ignore_ascii_case("outlook") {
            return "Token exchange failed: Microsoft rejected this OAuth app because it requires a client secret. Set MICROSOFT_CLIENT_SECRET in .env and restart Pebble Web.".to_string();
        }
        if provider.eq_ignore_ascii_case("gmail") {
            return "Token exchange failed: Google rejected this OAuth app because it requires a client secret. Set GOOGLE_CLIENT_SECRET in .env and restart Pebble Web.".to_string();
        }
    }
    format!("Token exchange failed: {detail}")
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

#[derive(Deserialize)]
struct StartOAuthArgs {
    provider: String,
    email: String,
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
    #[serde(rename = "error_description")]
    pub(crate) _error_description: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum OAuthFlowStatus {
    Processing { updated_at: i64 },
    Success { account: Account, updated_at: i64 },
    Error { message: String, updated_at: i64 },
}

impl OAuthFlowStatus {
    fn updated_at(&self) -> i64 {
        match self {
            Self::Processing { updated_at }
            | Self::Success { updated_at, .. }
            | Self::Error { updated_at, .. } => *updated_at,
        }
    }

    fn is_processing(&self) -> bool {
        matches!(self, Self::Processing { .. })
    }

    fn response_json(&self) -> Value {
        match self {
            Self::Processing { .. } => json!({ "status": "processing" }),
            Self::Success { account, .. } => json!({ "status": "success", "account": account }),
            Self::Error { message, .. } => json!({ "status": "error", "message": message }),
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct OAuthCallbackStatusArgs {
    state: String,
    #[serde(default)]
    cancel_pending: bool,
}

async fn set_oauth_flow_status(state: &AppStateRef, state_key: &str, status: OAuthFlowStatus) {
    state
        .oauth_flow_status
        .lock()
        .await
        .insert(state_key.to_string(), status);
}

async fn callback_page_flow_error(
    state: &AppStateRef,
    state_key: &str,
    message: String,
) -> Response {
    set_oauth_flow_status(
        state,
        state_key,
        OAuthFlowStatus::Error {
            message: message.clone(),
            updated_at: now_timestamp(),
        },
    )
    .await;
    callback_page_error(&message)
}

/// Report the server-side state of a browser OAuth flow. The opener uses this
/// at the 5-minute redirect boundary and after an early popup close so command
/// completion follows the server result instead of the popup lifetime.
pub(crate) async fn oauth_callback_status(
    State(state): State<AppStateRef>,
    headers: HeaderMap,
    Json(args): Json<OAuthCallbackStatusArgs>,
) -> Result<Json<Value>, ApiError> {
    crate::auth::require_auth(&state, &headers)?;
    let state_key = args.state.trim();
    if state_key.is_empty() {
        return Err(ApiError::BadRequest("OAuth callback status requires state".to_string()));
    }
    let now = now_timestamp();
    {
        let mut pending = state.oauth_pending.lock().await;
        match pending.get(state_key).map(|flow| flow.created_at) {
            Some(created_at) if created_at + PENDING_OAUTH_TTL_SECS > now => {
                if args.cancel_pending {
                    pending.remove(state_key);
                    return Ok(Json(json!({ "status": "expired" })));
                }
                return Ok(Json(json!({ "status": "pending" })));
            }
            Some(_) => {
                pending.remove(state_key);
            }
            None => {}
        }
    }

    let mut statuses = state.oauth_flow_status.lock().await;
    statuses.retain(|_, status| {
        status.is_processing()
            || status.updated_at() + OAUTH_TERMINAL_STATUS_RETENTION_SECS > now
    });
    if let Some(status) = statuses.get(state_key) {
        return Ok(Json(status.response_json()));
    }
    Ok(Json(json!({ "status": "expired" })))
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

    let now = now_timestamp();
    {
        let mut statuses = state.oauth_flow_status.lock().await;
        statuses.retain(|_, status| {
            status.is_processing()
                || status.updated_at() + OAUTH_TERMINAL_STATUS_RETENTION_SECS > now
        });
    }
    let mut pending = state.oauth_pending.lock().await;
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
        return callback_page_error("OAuth redirect failed: Authorization callback missing state");
    };
    let mut pending_guard = state.oauth_pending.lock().await;
    let pending = pending_guard.remove(state_key);
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
    set_oauth_flow_status(
        &state,
        state_key,
        OAuthFlowStatus::Processing {
            updated_at: now_timestamp(),
        },
    )
    .await;
    drop(pending_guard);
    if let Some(error) = query.error.as_deref() {
        return callback_page_flow_error(
            &state,
            state_key,
            format!("OAuth redirect failed: Authorization denied or missing code: {error}"),
        )
        .await;
    }
    let Some(code) = query.code.as_deref().filter(|value| !value.is_empty()) else {
        return callback_page_flow_error(
            &state,
            state_key,
            "OAuth redirect failed: Authorization denied or missing code: unknown".to_string(),
        )
        .await;
    };

    let Some(pkce_state) = pending.pkce_state.take() else {
        return callback_page_flow_error(
            &state,
            state_key,
            "OAuth state is invalid or already used".to_string(),
        )
        .await;
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
        Err(error) => {
            return callback_page_flow_error(
                &state,
                state_key,
                token_exchange_error_message(&pending.provider, &error),
            )
            .await;
        }
    };
    let identity =
        match fetch_userinfo(&pending.provider, &tokens.access_token, &pending.network).await {
            Ok(identity) => identity,
            Err(_error) if pending.provider == "gmail" => {
                (pending.email.clone(), pending.display_name.clone())
            }
            Err(error) => {
                return callback_page_flow_error(&state, state_key, error).await;
            }
        };

    let account = match persist_oauth_account(&state, &pending, &tokens, identity).await {
        Ok(account) => account,
        Err(error) => {
            return callback_page_flow_error(&state, state_key, error).await;
        }
    };
    set_oauth_flow_status(
        &state,
        state_key,
        OAuthFlowStatus::Success {
            account: account.clone(),
            updated_at: now_timestamp(),
        },
    )
    .await;
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
    let oauth_tokens = OAuthTokens {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        expires_at: tokens.expires_at,
        scopes: tokens.scopes.clone(),
    };
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let provider_type = provider_type(&provider).map_err(|e| e.to_string())?;
    run_blocking(move || {
        let accounts = store.list_accounts()?;
        let now = now_timestamp();
        let color = Some(default_account_color(&accounts, &email));
        let account = Account {
            id: new_id(),
            email,
            display_name,
            color,
            provider: provider_type,
            created_at: now,
            updated_at: now,
        };
        store.insert_account(&account)?;
        let result = (|| -> Result<(), PebbleError> {
            let stored = crate::commands::oauth::StoredOAuthAuthData::from_tokens(
                oauth_tokens,
                account_proxy,
            );
            crate::commands::oauth::persist_stored_oauth_auth_data_raw(
                &crypto,
                &store,
                &account.id,
                &stored,
            )?;
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

fn non_empty_json_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value[field]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
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
    let client = build_http_client(network)
        .map_err(|e| format!("Userinfo HTTP client failed: {e}"))?;
    let value: Value = client
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("Userinfo request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Userinfo request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Userinfo parse failed: {e}"))?;
    let email = match provider {
        "gmail" => non_empty_json_string(&value, "email"),
        "outlook" => non_empty_json_string(&value, "mail")
            .or_else(|| non_empty_json_string(&value, "userPrincipalName")),
        _ => None,
    }
    .ok_or_else(|| format!("{provider} user profile did not include a mailbox address"))?;
    let name = match provider {
        "outlook" => non_empty_json_string(&value, "displayName")
            .or_else(|| non_empty_json_string(&value, "name")),
        _ => non_empty_json_string(&value, "name")
            .or_else(|| non_empty_json_string(&value, "displayName")),
    }
    .unwrap_or_default();
    Ok((email.to_string(), name.to_string()))
}

fn normalize_provider(provider: &str) -> Result<String, String> {
    let normalized = provider.to_ascii_lowercase();
    if matches!(normalized.as_str(), "gmail" | "outlook") {
        Ok(normalized)
    } else {
        Err(format!("Unknown OAuth provider: {provider}"))
    }
}

fn provider_type(provider: &str) -> Result<ProviderType, PebbleError> {
    match provider {
        "gmail" => Ok(ProviderType::Gmail),
        "outlook" => Ok(ProviderType::Outlook),
        _ => Err(PebbleError::UnsupportedProvider(provider.to_string())),
    }
}

fn dotenv_lookup_from_str(contents: &str, key: &str) -> Option<String> {
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }

        let value = value.trim();
        let unquoted = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        return Some(unquoted.to_string());
    }
    None
}

fn dotenv_contents() -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join(".env"));
        candidates.push(current_dir.join("..").join(".env"));
    }
    candidates.push(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env"));
    candidates.push(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(".env"),
    );

    candidates
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
}

fn web_config_value_from_sources(
    key: &str,
    env_value: Option<&str>,
    dotenv_contents: Option<&str>,
    compile_value: Option<&str>,
    placeholder: &str,
) -> String {
    env_value
        .filter(|value| !is_placeholder(value))
        .map(ToOwned::to_owned)
        .or_else(|| {
            dotenv_contents
                .and_then(|contents| dotenv_lookup_from_str(contents, key))
                .filter(|value| !is_placeholder(value))
        })
        .or_else(|| {
            compile_value
                .filter(|value| !is_placeholder(value))
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| placeholder.to_string())
}

fn web_config_value(key: &str, compile_value: Option<&str>, placeholder: &str) -> String {
    let env_value = std::env::var(key).ok();
    let dotenv = dotenv_contents();
    web_config_value_from_sources(
        key,
        env_value.as_deref(),
        dotenv.as_deref(),
        compile_value,
        placeholder,
    )
}

fn web_config_optional_value(key: &str, compile_value: Option<&str>) -> Option<String> {
    let value = web_config_value(key, compile_value, "");
    if is_placeholder(&value) {
        None
    } else {
        Some(value)
    }
}

pub(crate) fn gmail_oauth_config() -> OAuthConfig {
    OAuthConfig {
        client_id: web_config_value(
            "GOOGLE_CLIENT_ID",
            option_env!("GOOGLE_CLIENT_ID"),
            "GOOGLE_CLIENT_ID_PLACEHOLDER",
        ),
        client_secret: web_config_optional_value(
            "GOOGLE_CLIENT_SECRET",
            option_env!("GOOGLE_CLIENT_SECRET"),
        ),
        auth_url: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
        token_url: "https://oauth2.googleapis.com/token".to_string(),
        scopes: vec![
            "https://mail.google.com/".to_string(),
            "https://www.googleapis.com/auth/userinfo.email".to_string(),
            "https://www.googleapis.com/auth/userinfo.profile".to_string(),
        ],
        redirect_port: 0,
    }
}

pub(crate) fn outlook_oauth_config() -> OAuthConfig {
    OAuthConfig {
        client_id: web_config_value(
            "MICROSOFT_CLIENT_ID",
            option_env!("MICROSOFT_CLIENT_ID"),
            "MICROSOFT_CLIENT_ID_PLACEHOLDER",
        ),
        client_secret: web_config_optional_value(
            "MICROSOFT_CLIENT_SECRET",
            option_env!("MICROSOFT_CLIENT_SECRET"),
        ),
        auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".to_string(),
        token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".to_string(),
        scopes: vec![
            "https://graph.microsoft.com/Mail.ReadWrite".to_string(),
            "https://graph.microsoft.com/Mail.Send".to_string(),
            "https://graph.microsoft.com/User.Read".to_string(),
            "offline_access".to_string(),
        ],
        redirect_port: 0,
    }
}

fn oauth_config_for_provider(provider: &str) -> Result<OAuthConfig, String> {
    let config = match provider {
        "gmail" => gmail_oauth_config(),
        "outlook" => outlook_oauth_config(),
        _ => return Err(format!("unsupported OAuth provider: {provider}")),
    };
    if is_placeholder(&config.client_id) {
        return Err(format!(
            "OAuth client_id for '{provider}' is not configured. Set the appropriate environment variable before starting the OAuth flow."
        ));
    }
    Ok(config)
}

fn oauth_redirect_url() -> Result<String, String> {
    let value = web_config_value(
        "PEBBLE_OAUTH_REDIRECT_URL",
        option_env!("PEBBLE_OAUTH_REDIRECT_URL"),
        "",
    );
    if value.trim().is_empty() {
        return Err("PEBBLE_OAUTH_REDIRECT_URL is required for Web OAuth".to_string());
    }
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
