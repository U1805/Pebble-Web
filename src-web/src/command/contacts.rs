use pebble_core::{ContactInput, KnownContact, VcardImportResult};
use serde_json::Value;

use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

pub async fn list_contacts(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        favorite_only: Option<bool>,
        #[serde(default)]
        limit: Option<i64>,
        #[serde(default)]
        offset: Option<i64>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_contacts args: {e}")))?;
    let store = state.store.clone();
    let contacts = run_blocking(move || {
        store.list_contacts(
            args.query.as_deref(),
            args.favorite_only.unwrap_or(false),
            args.limit.unwrap_or(50),
            args.offset.unwrap_or(0),
        )
    })
    .await?;
    serde_json::to_value(contacts).map_err(ApiError::from_serialize)
}

pub async fn get_contact_by_email(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let address: String = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_contact_by_email args: {e}")))?;
    let store = state.store.clone();
    let contact = run_blocking(move || store.get_contact_by_email(&address)).await?;
    serde_json::to_value(contact).map_err(ApiError::from_serialize)
}

pub async fn save_contact(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    // 与上游 Tauri 命令签名一致：前端 invoke 传 { input: ContactInput }
    let input: ContactInput =
        serde_json::from_value(args.get("input").cloned().unwrap_or(Value::Null))
            .map_err(|e| ApiError::BadRequest(format!("invalid save_contact args: {e}")))?;
    let store = state.store.clone();
    let contact = run_blocking(move || store.save_contact(&input)).await?;
    serde_json::to_value(contact).map_err(ApiError::from_serialize)
}

pub async fn delete_contact(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        contact_id: String,
        #[serde(default)]
        suppress_addresses: Option<bool>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid delete_contact args: {e}")))?;
    let contact_id = args.contact_id.trim().to_string();
    if contact_id.is_empty() {
        return Err(ApiError::BadRequest(
            "contact id must not be empty".to_string(),
        ));
    }
    let store = state.store.clone();
    run_blocking(move || {
        store.delete_contact(&contact_id, args.suppress_addresses.unwrap_or(false))
    })
    .await?;
    Ok(Value::Null)
}

pub async fn set_contact_favorite(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        contact_id: String,
        is_favorite: bool,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid set_contact_favorite args: {e}")))?;
    let contact_id = args.contact_id.trim().to_string();
    if contact_id.is_empty() {
        return Err(ApiError::BadRequest(
            "contact id must not be empty".to_string(),
        ));
    }
    let store = state.store.clone();
    run_blocking(move || store.set_contact_favorite(&contact_id, args.is_favorite)).await?;
    Ok(Value::Null)
}

pub async fn search_contact_suggestions(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        query: String,
        #[serde(default)]
        limit: Option<i64>,
    }
    let args: Args = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid search_contact_suggestions args: {e}"))
    })?;
    let store = state.store.clone();
    let suggestions = run_blocking(move || {
        store.search_contact_suggestions(&args.account_id, &args.query, args.limit.unwrap_or(20))
    })
    .await?;
    serde_json::to_value(suggestions).map_err(ApiError::from_serialize)
}

pub async fn suppress_contact_suggestion(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    let address: String = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid suppress_contact_suggestion args: {e}"))
    })?;
    let store = state.store.clone();
    run_blocking(move || store.suppress_contact_suggestion(&address)).await?;
    Ok(Value::Null)
}

pub async fn import_contacts_vcard(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let data: String = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid import_contacts_vcard args: {e}")))?;
    let store = state.store.clone();
    let result: VcardImportResult =
        run_blocking(move || store.import_contacts_vcard(&data)).await?;
    serde_json::to_value(result).map_err(ApiError::from_serialize)
}

pub async fn export_contacts_vcard(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let store = state.store.clone();
    let vcard = run_blocking(move || store.export_contacts_vcard()).await?;
    serde_json::to_value(vcard).map_err(ApiError::from_serialize)
}

pub async fn search_contacts(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        query: String,
        #[serde(default)]
        limit: Option<i64>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid search_contacts args: {e}")))?;
    let store = state.store.clone();
    let contacts: Vec<KnownContact> = run_blocking(move || {
        store.list_known_contacts(&args.account_id, &args.query, args.limit.unwrap_or(20))
    })
    .await?;
    serde_json::to_value(contacts).map_err(ApiError::from_serialize)
}
