use std::collections::HashMap;

use pebble_core::{now_timestamp, KanbanCard, KanbanColumn, PebbleError};
use serde::Deserialize;
use serde_json::Value;

use crate::blocking::run_blocking;
use crate::commands::encrypted_store::{
    load_secure_user_data, lock_secure_user_data_key, store_secure_user_data,
};
use crate::error::ApiError;
use crate::state::AppStateRef;

pub(crate) const KANBAN_CONTEXT_NOTES_KEY: &str = "kanban_context_notes";

fn load_context_notes(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
) -> Result<HashMap<String, String>, PebbleError> {
    let Some(plaintext) = load_secure_user_data(crypto, store, KANBAN_CONTEXT_NOTES_KEY)? else {
        return Ok(HashMap::new());
    };
    serde_json::from_slice(&plaintext).map_err(|e| {
        PebbleError::Internal(format!(
            "Invalid secure user data for {KANBAN_CONTEXT_NOTES_KEY}: {e}"
        ))
    })
}

fn normalize_context_notes(notes: HashMap<String, String>) -> HashMap<String, String> {
    notes
        .into_iter()
        .filter_map(|(message_id, note)| {
            let message_id = message_id.trim().to_string();
            if message_id.is_empty() || note.is_empty() {
                None
            } else {
                Some((message_id, note))
            }
        })
        .collect()
}

fn store_context_notes(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
    notes: HashMap<String, String>,
) -> Result<HashMap<String, String>, PebbleError> {
    let notes = normalize_context_notes(notes);
    if notes.is_empty() {
        store.delete_secure_user_data(KANBAN_CONTEXT_NOTES_KEY)?;
    } else {
        let plaintext = serde_json::to_vec(&notes).map_err(|e| {
            PebbleError::Internal(format!("Failed to serialize secure user data: {e}"))
        })?;
        store_secure_user_data(crypto, store, KANBAN_CONTEXT_NOTES_KEY, &plaintext)?;
    }
    Ok(notes)
}

pub(crate) fn load_kanban_context_notes_for_state(
    state: &AppStateRef,
) -> Result<HashMap<String, String>, PebbleError> {
    load_context_notes(&state.store, &state.crypto)
}

pub(crate) fn encrypt_kanban_context_notes_for_state(
    state: &AppStateRef,
    notes: HashMap<String, String>,
) -> Result<Option<Vec<u8>>, PebbleError> {
    let notes = normalize_context_notes(notes);
    if notes.is_empty() {
        return Ok(None);
    }
    let plaintext = serde_json::to_vec(&notes)
        .map_err(|e| PebbleError::Internal(format!("Failed to serialize secure user data: {e}")))?;
    crate::commands::encrypted_store::encrypt_secure_user_data(
        &state.crypto,
        KANBAN_CONTEXT_NOTES_KEY,
        &plaintext,
    )
    .map(Some)
}

pub async fn move_to_kanban(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        message_id: String,
        column: KanbanColumn,
        #[serde(default)]
        position: Option<i32>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid move_to_kanban args: {e}")))?;
    let now = now_timestamp();
    let card = KanbanCard {
        message_id: args.message_id,
        column: args.column,
        position: args.position.unwrap_or(0),
        created_at: now,
        updated_at: now,
    };
    let store = state.store.clone();
    run_blocking(move || store.upsert_kanban_card(&card)).await?;
    Ok(Value::Null)
}

pub async fn list_kanban_cards(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        #[serde(default)]
        column: Option<KanbanColumn>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_kanban_cards args: {e}")))?;
    let store = state.store.clone();
    let cards = run_blocking(move || store.list_kanban_cards(args.column.as_ref())).await?;
    serde_json::to_value(cards).map_err(ApiError::from_serialize)
}

pub async fn remove_from_kanban(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        message_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid remove_from_kanban args: {e}")))?;
    let store = state.store.clone();
    run_blocking(move || store.delete_kanban_card(&args.message_id)).await?;
    Ok(Value::Null)
}

pub async fn list_kanban_context_notes(
    state: AppStateRef,
    _args: Value,
) -> Result<Value, ApiError> {
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let notes = run_blocking(move || load_context_notes(&store, &crypto)).await?;
    serde_json::to_value(notes).map_err(ApiError::from_serialize)
}

pub async fn set_kanban_context_note(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        message_id: String,
        note: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid set_kanban_context_note args: {e}")))?;
    let _guard = lock_secure_user_data_key(&state, KANBAN_CONTEXT_NOTES_KEY).await;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let notes = run_blocking(move || {
        let mut notes = load_context_notes(&store, &crypto)?;
        let message_id = args.message_id.trim().to_string();
        if message_id.is_empty() || args.note.is_empty() {
            notes.remove(&message_id);
        } else {
            notes.insert(message_id, args.note);
        }
        store_context_notes(&store, &crypto, notes)
    })
    .await?;
    serde_json::to_value(notes).map_err(ApiError::from_serialize)
}

pub async fn merge_kanban_context_notes(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        notes: HashMap<String, String>,
    }
    let args: Args = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid merge_kanban_context_notes args: {e}"))
    })?;
    let _guard = lock_secure_user_data_key(&state, KANBAN_CONTEXT_NOTES_KEY).await;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let notes = run_blocking(move || {
        let mut current = load_context_notes(&store, &crypto)?;
        for (message_id, note) in normalize_context_notes(args.notes) {
            current.entry(message_id).or_insert(note);
        }
        store_context_notes(&store, &crypto, current)
    })
    .await?;
    serde_json::to_value(notes).map_err(ApiError::from_serialize)
}
