use pebble_core::{new_id, now_timestamp, PebbleError};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use crate::blocking::run_blocking;
use crate::commands::encrypted_store::{
    load_secure_user_data, lock_secure_user_data_key, store_secure_user_data,
};
use crate::error::ApiError;
use crate::state::AppStateRef;

const EMAIL_TEMPLATES_KEY: &str = "email_templates";
const EMAIL_SIGNATURES_KEY: &str = "email_signatures";


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EmailTemplate {
    pub id: String,
    pub name: String,
    pub subject: String,
    pub body: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveEmailTemplateRequest {
    name: String,
    subject: String,
    body: String,
    #[serde(default)]
    deduplicate_by_content: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct SaveEmailTemplateArgs {
    template: SaveEmailTemplateRequest,
}

fn decrypt_json<T: DeserializeOwned>(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
    key: &str,
) -> Result<Option<T>, PebbleError> {
    let Some(plaintext) = load_secure_user_data(crypto, store, key)? else {
        return Ok(None);
    };
    serde_json::from_slice(&plaintext)
        .map(Some)
        .map_err(|e| PebbleError::Internal(format!("Invalid secure user data for {key}: {e}")))
}

fn encrypt_json<T: Serialize>(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
    key: &str,
    value: &T,
) -> Result<(), PebbleError> {
    let plaintext = serde_json::to_vec(value)
        .map_err(|e| PebbleError::Internal(format!("Failed to serialize secure user data: {e}")))?;
    store_secure_user_data(crypto, store, key, &plaintext)
}

fn normalize_template_input(input: SaveEmailTemplateRequest) -> SaveEmailTemplateRequest {
    SaveEmailTemplateRequest {
        name: input.name.trim().to_string(),
        subject: input.subject,
        body: input.body,
        deduplicate_by_content: input.deduplicate_by_content,
    }
}

fn save_template_raw(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
    input: SaveEmailTemplateRequest,
) -> Result<EmailTemplate, PebbleError> {
    let input = normalize_template_input(input);
    if input.name.is_empty() {
        return Err(PebbleError::Validation(
            "Template name cannot be empty".to_string(),
        ));
    }

    let mut templates: Vec<EmailTemplate> =
        decrypt_json(store, crypto, EMAIL_TEMPLATES_KEY)?.unwrap_or_default();
    if input.deduplicate_by_content {
        if let Some(existing) = templates.iter().find(|existing| {
            existing.name == input.name
                && existing.subject == input.subject
                && existing.body == input.body
        }) {
            return Ok(existing.clone());
        }
    }

    let saved = EmailTemplate {
        id: new_id(),
        name: input.name,
        subject: input.subject,
        body: input.body,
        created_at: now_timestamp(),
    };
    templates.push(saved.clone());
    encrypt_json(store, crypto, EMAIL_TEMPLATES_KEY, &templates)?;
    Ok(saved)
}

fn delete_template_raw(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
    id: &str,
) -> Result<(), PebbleError> {
    let mut templates: Vec<EmailTemplate> =
        decrypt_json(store, crypto, EMAIL_TEMPLATES_KEY)?.unwrap_or_default();
    templates.retain(|template| template.id != id);
    if templates.is_empty() {
        store.delete_secure_user_data(EMAIL_TEMPLATES_KEY)
    } else {
        encrypt_json(store, crypto, EMAIL_TEMPLATES_KEY, &templates)
    }
}

fn set_signature_raw(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
    account_id: String,
    signature: String,
) -> Result<(), PebbleError> {
    let mut signatures: HashMap<String, String> =
        decrypt_json(store, crypto, EMAIL_SIGNATURES_KEY)?.unwrap_or_default();
    // Preserve an explicit empty value as a tombstone so a delayed legacy
    // migration cannot restore a signature the user deliberately cleared.
    signatures.insert(account_id, signature);
    encrypt_json(store, crypto, EMAIL_SIGNATURES_KEY, &signatures)
}

fn migrate_signature_raw(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
    account_id: String,
    signature: String,
) -> Result<String, PebbleError> {
    let mut signatures: HashMap<String, String> =
        decrypt_json(store, crypto, EMAIL_SIGNATURES_KEY)?.unwrap_or_default();
    if let Some(current) = signatures.get(&account_id) {
        return Ok(current.clone());
    }
    if signature.trim().is_empty() {
        return Ok(String::new());
    }
    signatures.insert(account_id, signature.clone());
    encrypt_json(store, crypto, EMAIL_SIGNATURES_KEY, &signatures)?;
    Ok(signature)
}

pub async fn list_email_templates(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let templates: Vec<EmailTemplate> = run_blocking(move || {
        Ok(decrypt_json(&store, &crypto, EMAIL_TEMPLATES_KEY)?.unwrap_or_default())
    })
    .await?;
    serde_json::to_value(templates).map_err(ApiError::from_serialize)
}

pub async fn save_email_template(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: SaveEmailTemplateArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid save_email_template args: {e}")))?;
    let _guard = lock_secure_user_data_key(&state, EMAIL_TEMPLATES_KEY).await;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let template = run_blocking(move || save_template_raw(&store, &crypto, args.template)).await?;
    serde_json::to_value(template).map_err(ApiError::from_serialize)
}

pub async fn delete_email_template(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid delete_email_template args: {e}")))?;
    let _guard = lock_secure_user_data_key(&state, EMAIL_TEMPLATES_KEY).await;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    run_blocking(move || delete_template_raw(&store, &crypto, &args.id)).await?;
    Ok(Value::Null)
}

pub async fn get_email_signature(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        account_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_email_signature args: {e}")))?;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let signature = run_blocking(move || {
        let signatures: HashMap<String, String> =
            decrypt_json(&store, &crypto, EMAIL_SIGNATURES_KEY)?.unwrap_or_default();
        Ok(signatures
            .get(&args.account_id)
            .cloned()
            .unwrap_or_default())
    })
    .await?;
    Ok(Value::String(signature))
}

pub async fn set_email_signature(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        account_id: String,
        signature: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid set_email_signature args: {e}")))?;
    let _guard = lock_secure_user_data_key(&state, EMAIL_SIGNATURES_KEY).await;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    run_blocking(move || set_signature_raw(&store, &crypto, args.account_id, args.signature))
        .await?;
    Ok(Value::Null)
}

pub async fn migrate_email_signature_if_absent(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        account_id: String,
        signature: String,
    }
    let args: Args = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!(
            "invalid migrate_email_signature_if_absent args: {e}"
        ))
    })?;
    let _guard = lock_secure_user_data_key(&state, EMAIL_SIGNATURES_KEY).await;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let signature = run_blocking(move || {
        migrate_signature_raw(&store, &crypto, args.account_id, args.signature)
    })
    .await?;
    Ok(Value::String(signature))
}
