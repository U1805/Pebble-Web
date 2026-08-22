use pebble_core::{now_timestamp, PebbleError, TranslateConfig};
use pebble_translate::types::{TranslateProviderConfig, TranslateResult};
use pebble_translate::TranslateService;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::command::network::get_global_proxy_raw;
use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

const ACTIVE_TRANSLATE_CONFIG_ID: &str = "active";
const TRANSLATE_CONFIG_PURPOSE: &str = "translate_config.config";

fn decrypt_config(
    crypto: &pebble_crypto::CryptoService,
    store: &pebble_store::Store,
    stored: &str,
) -> Result<String, PebbleError> {
    if serde_json::from_str::<Value>(stored).is_ok() {
        let encrypted = encrypt_config(crypto, stored)?;
        store.compare_exchange_translate_config_blob(stored, &encrypted)?;
        return Ok(stored.to_string());
    }

    let encrypted = hex::decode(stored)
        .map_err(|e| PebbleError::Internal(format!("Invalid translate config encoding: {e}")))?;
    let needs_migration = pebble_crypto::CryptoService::ciphertext_needs_migration(&encrypted);
    let plaintext = crypto.decrypt_for(
        TRANSLATE_CONFIG_PURPOSE,
        ACTIVE_TRANSLATE_CONFIG_ID,
        &encrypted,
    )?;
    let plaintext = String::from_utf8(plaintext)
        .map_err(|e| PebbleError::Internal(format!("Invalid UTF-8 in translate config: {e}")))?;
    if needs_migration {
        let replacement = encrypt_config(crypto, &plaintext)?;
        store.compare_exchange_translate_config_blob(stored, &replacement)?;
    }
    Ok(plaintext)
}

fn encrypt_config(
    crypto: &pebble_crypto::CryptoService,
    plaintext: &str,
) -> Result<String, PebbleError> {
    let encrypted = crypto.encrypt_for(
        TRANSLATE_CONFIG_PURPOSE,
        ACTIVE_TRANSLATE_CONFIG_ID,
        plaintext.as_bytes(),
    )?;
    Ok(hex::encode(encrypted))
}

fn validate_provider_config(config: &TranslateProviderConfig) -> Result<(), PebbleError> {
    match config {
        TranslateProviderConfig::DeepLX { endpoint }
        | TranslateProviderConfig::GenericApi { endpoint, .. }
        | TranslateProviderConfig::LLM { endpoint, .. } => validate_translate_url(endpoint),
        TranslateProviderConfig::DeepL { .. } => Ok(()),
    }
}

fn validate_translate_url(url: &str) -> Result<(), PebbleError> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(after_scheme) = url.strip_prefix("http://") {
        let authority = after_scheme.split('/').next().unwrap_or("");
        // IPv6 literals contain colons, so preserve a bracketed host before
        // stripping an optional port from ordinary hostnames.
        let host = if let Some(end) = authority.find(']') {
            &authority[..=end]
        } else {
            authority.split(':').next().unwrap_or("")
        };
        if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
            return Ok(());
        }
        return Err(PebbleError::Validation(
            "Only HTTPS URLs are allowed for remote services".to_string(),
        ));
    }
    Err(PebbleError::Validation(
        "Unsupported URL scheme".to_string(),
    ))
}

fn parse_provider_config(config: &str) -> Result<TranslateProviderConfig, PebbleError> {
    let provider_config: TranslateProviderConfig = serde_json::from_str(config)
        .map_err(|e| PebbleError::Translate(format!("Invalid config: {e}")))?;
    validate_provider_config(&provider_config)?;
    Ok(provider_config)
}

pub async fn translate_text(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        text: String,
        from_lang: String,
        to_lang: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid translate_text args: {e}")))?;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let (provider_config, proxy) = run_blocking(move || {
        let config = store
            .get_translate_config()?
            .ok_or_else(|| PebbleError::Translate("No translate engine configured".to_string()))?;
        if !config.is_enabled {
            return Err(PebbleError::Translate(
                "Translation is disabled".to_string(),
            ));
        }
        let decrypted = decrypt_config(&crypto, &store, &config.config)?;
        let provider_config = parse_provider_config(&decrypted)?;
        let proxy = get_global_proxy_raw(&store, &crypto)?;
        Ok((provider_config, proxy))
    })
    .await?;

    let result: TranslateResult = TranslateService::translate_with_proxy(
        &provider_config,
        proxy.as_ref(),
        &args.text,
        &args.from_lang,
        &args.to_lang,
    )
    .await?;
    serde_json::to_value(result).map_err(ApiError::from_serialize)
}

pub async fn get_translate_config(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let config = run_blocking(move || {
        let Some(mut config) = store.get_translate_config()? else {
            return Ok(None);
        };
        config.config = decrypt_config(&crypto, &store, &config.config)?;
        Ok(Some(config))
    })
    .await?;
    serde_json::to_value(config).map_err(ApiError::from_serialize)
}

pub async fn save_translate_config(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        provider_type: String,
        config: String,
        is_enabled: bool,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid save_translate_config args: {e}")))?;
    parse_provider_config(&args.config)?;

    let store = state.store.clone();
    let crypto = state.crypto.clone();
    run_blocking(move || {
        let encrypted = encrypt_config(&crypto, &args.config)?;
        let now = now_timestamp();
        store.save_translate_config(&TranslateConfig {
            id: ACTIVE_TRANSLATE_CONFIG_ID.to_string(),
            provider_type: args.provider_type,
            config: encrypted,
            is_enabled: args.is_enabled,
            created_at: now,
            updated_at: now,
        })
    })
    .await?;
    Ok(Value::Null)
}

pub async fn test_translate_connection(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        config: String,
    }
    let args: Args = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid test_translate_connection args: {e}"))
    })?;
    let config = parse_provider_config(&args.config)?;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let proxy = run_blocking(move || get_global_proxy_raw(&store, &crypto)).await?;
    let result =
        TranslateService::translate_with_proxy(&config, proxy.as_ref(), "Hello", "en", "zh")
            .await?;
    Ok(json!(result.translated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_url_allows_https_and_local_http_only() {
        for url in [
            "https://translate.example.com/api",
            "http://localhost:1188/translate",
            "http://127.0.0.1:1188/translate",
            "http://[::1]:1188/translate",
        ] {
            assert!(
                validate_translate_url(url).is_ok(),
                "expected URL to pass: {url}"
            );
        }
    }

    #[test]
    fn translate_url_rejects_remote_http_and_unknown_schemes() {
        for url in [
            "http://translate.example.com/api",
            "ftp://translate.example.com",
        ] {
            let error = validate_translate_url(url).unwrap_err();
            assert!(error.to_string().contains("HTTPS") || error.to_string().contains("scheme"));
        }
    }

    #[test]
    fn provider_config_parser_validates_endpoint_and_shape() {
        let valid = r#"{"type":"deeplx","endpoint":"http://localhost:1188/translate"}"#;
        assert!(matches!(
            parse_provider_config(valid).unwrap(),
            TranslateProviderConfig::DeepLX { .. }
        ));

        let invalid_endpoint =
            r#"{"type":"deeplx","endpoint":"http://translate.example.com/translate"}"#;
        assert!(parse_provider_config(invalid_endpoint).is_err());
        assert!(parse_provider_config("not-json").is_err());
    }

    #[test]
    fn deepl_config_does_not_require_endpoint_validation() {
        let config = r#"{"type":"deepl","api_key":"secret","use_free_api":true}"#;
        assert!(matches!(
            parse_provider_config(config).unwrap(),
            TranslateProviderConfig::DeepL { .. }
        ));
    }
}
