use pebble_core::{now_timestamp, PebbleError, TranslateConfig};
use pebble_translate::types::{TranslateProviderConfig, TranslateResult};
use pebble_translate::TranslateService;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::blocking::run_blocking;
use crate::commands::encrypted_store::{ACTIVE_TRANSLATE_CONFIG_ID, TRANSLATE_CONFIG_PURPOSE};
use crate::commands::network::get_global_proxy_raw;
use crate::error::ApiError;
use crate::state::AppStateRef;

fn hex_decode(s: &str) -> Result<Vec<u8>, PebbleError> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(PebbleError::Internal(
            "Invalid hex string length".to_string(),
        ));
    }
    bytes
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])
                .ok_or_else(|| PebbleError::Internal("Invalid hex digit".to_string()))?;
            let low = hex_nibble(pair[1])
                .ok_or_else(|| PebbleError::Internal("Invalid hex digit".to_string()))?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn decrypt_config(
    crypto: &pebble_crypto::CryptoService,
    store: &pebble_store::Store,
    stored: &str,
) -> Result<String, PebbleError> {
    if serde_json::from_str::<Value>(stored).is_ok() {
        let encrypted = encrypt_config(crypto, stored)?;
        store.compare_exchange_translate_config_blob(stored, &encrypted)?;
        return Ok(stored.to_string());
    }

    let encrypted = hex_decode(stored)?;
    let needs_migration = pebble_crypto::CryptoService::ciphertext_needs_migration(&encrypted);
    let plaintext = crypto.decrypt_for(
        TRANSLATE_CONFIG_PURPOSE,
        ACTIVE_TRANSLATE_CONFIG_ID,
        &encrypted,
    )?;
    let plaintext = String::from_utf8(plaintext)
        .map_err(|e| PebbleError::Internal(format!("Invalid UTF-8 in decrypted config: {e}")))?;
    if needs_migration {
        let replacement = encrypt_config(crypto, &plaintext)?;
        store.compare_exchange_translate_config_blob(stored, &replacement)?;
    }
    Ok(plaintext)
}

pub(crate) fn encrypt_config(
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
        let host = after_scheme
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");
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
        let proxy = get_global_proxy_raw(&crypto, &store)?;
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
    let proxy = run_blocking(move || get_global_proxy_raw(&crypto, &store)).await?;
    let result =
        TranslateService::translate_with_proxy(&config, proxy.as_ref(), "Hello", "en", "zh")
            .await?;
    Ok(json!(result.translated))
}
