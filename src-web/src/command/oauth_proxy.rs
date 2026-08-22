use pebble_core::{Account, HttpProxyConfig, PebbleError, ProviderType};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::command::accounts::{proxy_config_from_parts, AccountProxyMode, AccountProxySetting};
use crate::command::run_blocking;
use crate::credentials;
use crate::error::ApiError;
use crate::state::AppStateRef;

static OAUTH_PROXY_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountProxyArgs {
    account_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateOAuthProxyArgs {
    account_id: String,
    #[serde(default)]
    proxy_host: Option<String>,
    #[serde(default)]
    proxy_port: Option<u16>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateOAuthProxySettingArgs {
    account_id: String,
    mode: AccountProxyMode,
    #[serde(default)]
    proxy_host: Option<String>,
    #[serde(default)]
    proxy_port: Option<u16>,
}

fn ensure_oauth_account(account: Option<Account>, account_id: &str) -> Result<Account, ApiError> {
    let account =
        account.ok_or_else(|| ApiError::NotFound(format!("account not found: {account_id}")))?;
    if !matches!(
        account.provider,
        ProviderType::Gmail | ProviderType::Outlook
    ) {
        return Err(ApiError::from_pebble(PebbleError::UnsupportedProvider(
            "OAuth proxy commands require a Gmail or Outlook account".to_string(),
        )));
    }
    Ok(account)
}

fn load_oauth_auth_value(state: &AppStateRef, account_id: &str) -> Result<Value, ApiError> {
    let bytes = credentials::load_account_auth_data(&state.crypto, &state.store, account_id)
        .map_err(ApiError::Internal)?
        .ok_or_else(|| {
            ApiError::Internal(format!("No auth data found for account {account_id}"))
        })?;
    serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::Internal(format!("Failed to parse OAuth auth data: {e}")))
}

fn setting_from_auth_value(value: &Value) -> Result<AccountProxySetting, ApiError> {
    let mode = value
        .get("proxy_mode")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| ApiError::Internal(format!("Failed to parse OAuth proxy mode: {e}")))?
        .unwrap_or_default();
    let proxy = match value.get("proxy") {
        Some(value) if !value.is_null() => Some(
            serde_json::from_value::<HttpProxyConfig>(value.clone())
                .map_err(|e| ApiError::Internal(format!("Failed to parse OAuth proxy: {e}")))?,
        ),
        _ => None,
    };
    let mode = if matches!(mode, AccountProxyMode::Inherit) && proxy.is_some() {
        AccountProxyMode::Custom
    } else {
        mode
    };
    Ok(AccountProxySetting {
        proxy: if matches!(mode, AccountProxyMode::Custom) {
            proxy
        } else {
            None
        },
        mode,
    })
}

fn set_auth_proxy_setting(
    value: &mut Value,
    setting: &AccountProxySetting,
) -> Result<(), ApiError> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| ApiError::Internal("OAuth auth data must be a JSON object".to_string()))?;
    match setting.mode {
        AccountProxyMode::Inherit => {
            object.remove("proxy_mode");
            object.remove("proxy");
        }
        AccountProxyMode::Disabled => {
            object.insert("proxy_mode".to_string(), json!("disabled"));
            object.remove("proxy");
        }
        AccountProxyMode::Custom => {
            let proxy = setting.proxy.as_ref().ok_or_else(|| {
                ApiError::BadRequest("custom OAuth proxy requires host and port".to_string())
            })?;
            object.insert("proxy_mode".to_string(), json!("custom"));
            object.insert(
                "proxy".to_string(),
                serde_json::to_value(proxy).map_err(ApiError::from_serialize)?,
            );
        }
    }
    Ok(())
}

fn update_oauth_proxy_setting_raw(
    state: &AppStateRef,
    account_id: &str,
    setting: AccountProxySetting,
) -> Result<(), ApiError> {
    let mut value = load_oauth_auth_value(state, account_id)?;
    set_auth_proxy_setting(&mut value, &setting)?;
    let serialized = serde_json::to_vec(&value).map_err(ApiError::from_serialize)?;
    credentials::store_account_auth_data(&state.crypto, &state.store, account_id, &serialized)
        .map_err(ApiError::Internal)
}

pub async fn get_oauth_account_proxy(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: AccountProxyArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_oauth_account_proxy args: {e}")))?;
    let account = state
        .store
        .get_account(&args.account_id)
        .map_err(ApiError::from_store)?;
    ensure_oauth_account(account, &args.account_id)?;
    let setting = setting_from_auth_value(&load_oauth_auth_value(&state, &args.account_id)?)?;
    serde_json::to_value(setting.proxy).map_err(ApiError::from_serialize)
}

pub async fn get_oauth_account_proxy_setting(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    let args: AccountProxyArgs = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid get_oauth_account_proxy_setting args: {e}"))
    })?;
    let account = state
        .store
        .get_account(&args.account_id)
        .map_err(ApiError::from_store)?;
    ensure_oauth_account(account, &args.account_id)?;
    let setting = setting_from_auth_value(&load_oauth_auth_value(&state, &args.account_id)?)?;
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
        .map_err(ApiError::from_pebble)?
        .map(|proxy| HttpProxyConfig {
            host: proxy.host,
            port: proxy.port,
        });
    let setting = AccountProxySetting {
        mode: if proxy.is_some() {
            AccountProxyMode::Custom
        } else {
            AccountProxyMode::Inherit
        },
        proxy,
    };
    update_oauth_account_proxy_setting_value(state, args.account_id, setting).await?;
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
    let proxy = proxy_config_from_parts(args.proxy_host, args.proxy_port, "OAuth proxy")
        .map_err(ApiError::from_pebble)?
        .map(|proxy| HttpProxyConfig {
            host: proxy.host,
            port: proxy.port,
        });
    let proxy = match args.mode {
        AccountProxyMode::Custom => Some(proxy.ok_or_else(|| {
            ApiError::BadRequest("custom OAuth proxy requires host and port".to_string())
        })?),
        AccountProxyMode::Inherit | AccountProxyMode::Disabled => None,
    };
    update_oauth_account_proxy_setting_value(
        state,
        args.account_id,
        AccountProxySetting {
            mode: args.mode,
            proxy,
        },
    )
    .await?;
    Ok(Value::Null)
}

async fn update_oauth_account_proxy_setting_value(
    state: AppStateRef,
    account_id: String,
    setting: AccountProxySetting,
) -> Result<(), ApiError> {
    let _guard = OAUTH_PROXY_LOCK.lock().await;
    let account = state
        .store
        .get_account(&account_id)
        .map_err(ApiError::from_store)?;
    ensure_oauth_account(account, &account_id)?;
    run_blocking(move || {
        update_oauth_proxy_setting_raw(&state, &account_id, setting)
            .map_err(|error| PebbleError::Internal(format!("{error:?}")))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth, config::Config, credentials, state::AppState};
    use pebble_core::{new_id, now_timestamp, Account};

    #[test]
    fn inherited_proxy_with_legacy_value_normalizes_to_custom() {
        let setting = setting_from_auth_value(&json!({
            "access_token": "token",
            "proxy": { "host": "127.0.0.1", "port": 7890 }
        }))
        .unwrap();
        assert_eq!(setting.mode, AccountProxyMode::Custom);
        assert_eq!(setting.proxy.unwrap().port, 7890);
    }

    #[test]
    fn disabled_proxy_never_exposes_stale_proxy() {
        let setting = setting_from_auth_value(&json!({
            "proxy_mode": "disabled",
            "proxy": { "host": "127.0.0.1", "port": 7890 }
        }))
        .unwrap();
        assert_eq!(setting.mode, AccountProxyMode::Disabled);
        assert!(setting.proxy.is_none());
    }

    #[tokio::test]
    async fn oauth_proxy_updates_preserve_encrypted_token_payload() {
        let data_dir = std::env::temp_dir().join(format!("pw-oauth-proxy-{}", new_id()));
        let state = AppState::init(Config {
            port: 0,
            data_dir: data_dir.clone(),
            password_hash: auth::hash_password("test-password").unwrap(),
            jwt_secret: "this-is-a-real-secret-with-32-plus-chars".to_string(),
            sync_interval_secs: 3600,
            static_dir: std::path::PathBuf::from("./dist"),
        })
        .unwrap();
        let now = now_timestamp();
        let account = Account {
            id: "oauth-account-1".to_string(),
            email: "oauth@example.com".to_string(),
            display_name: "OAuth".to_string(),
            color: None,
            provider: ProviderType::Gmail,
            created_at: now,
            updated_at: now,
        };
        state.store.insert_account(&account).unwrap();
        credentials::store_account_auth_data(
            &state.crypto,
            &state.store,
            &account.id,
            br#"{"access_token":"access","refresh_token":"refresh","scopes":["mail"]}"#,
        )
        .unwrap();

        let initial = get_oauth_account_proxy_setting(
            state.clone(),
            json!({ "accountId": account.id.clone() }),
        )
        .await
        .unwrap();
        assert_eq!(initial["mode"], "inherit");
        assert!(initial["proxy"].is_null());

        update_oauth_account_proxy(
            state.clone(),
            json!({ "accountId": account.id.clone(), "proxyHost": "127.0.0.1", "proxyPort": 7890 }),
        )
        .await
        .unwrap();
        let setting = get_oauth_account_proxy_setting(
            state.clone(),
            json!({ "accountId": account.id.clone() }),
        )
        .await
        .unwrap();
        assert_eq!(setting["mode"], "custom");
        assert_eq!(setting["proxy"]["port"], 7890);

        let stored = credentials::load_account_auth_data(&state.crypto, &state.store, &account.id)
            .unwrap()
            .unwrap();
        let stored: Value = serde_json::from_slice(&stored).unwrap();
        assert_eq!(stored["access_token"], "access");
        assert_eq!(stored["refresh_token"], "refresh");

        update_oauth_account_proxy_setting(
            state.clone(),
            json!({ "accountId": account.id.clone(), "mode": "disabled" }),
        )
        .await
        .unwrap();
        let setting = get_oauth_account_proxy_setting(
            state.clone(),
            json!({ "accountId": account.id.clone() }),
        )
        .await
        .unwrap();
        assert_eq!(setting["mode"], "disabled");
        assert!(setting["proxy"].is_null());

        drop(state);
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
