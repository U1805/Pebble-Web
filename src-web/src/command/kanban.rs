use std::collections::HashMap;

use pebble_core::{now_timestamp, KanbanCard, KanbanColumn, PebbleError};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::command::run_blocking;
use crate::credentials::SECURE_USER_DATA_PURPOSE;
use crate::error::ApiError;
use crate::state::AppStateRef;

const KANBAN_CONTEXT_NOTES_KEY: &str = "kanban_context_notes";
static KANBAN_CONTEXT_NOTES_LOCK: Mutex<()> = Mutex::const_new(());

fn load_context_notes(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
) -> Result<HashMap<String, String>, PebbleError> {
    let Some(encrypted) = store.get_secure_user_data(KANBAN_CONTEXT_NOTES_KEY)? else {
        return Ok(HashMap::new());
    };
    let plaintext = crypto.decrypt_for(
        SECURE_USER_DATA_PURPOSE,
        KANBAN_CONTEXT_NOTES_KEY,
        &encrypted,
    )?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse Kanban notes: {e}")))
}

fn store_context_notes(
    store: &pebble_store::Store,
    crypto: &pebble_crypto::CryptoService,
    notes: &HashMap<String, String>,
) -> Result<(), PebbleError> {
    if notes.is_empty() {
        return store.delete_secure_user_data(KANBAN_CONTEXT_NOTES_KEY);
    }
    let plaintext = serde_json::to_vec(notes)
        .map_err(|e| PebbleError::Internal(format!("Failed to serialize Kanban notes: {e}")))?;
    let encrypted = crypto.encrypt_for(
        SECURE_USER_DATA_PURPOSE,
        KANBAN_CONTEXT_NOTES_KEY,
        &plaintext,
    )?;
    store.set_secure_user_data(KANBAN_CONTEXT_NOTES_KEY, &encrypted)
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
    let _guard = KANBAN_CONTEXT_NOTES_LOCK.lock().await;
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
        store_context_notes(&store, &crypto, &notes)?;
        Ok(notes)
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
    let _guard = KANBAN_CONTEXT_NOTES_LOCK.lock().await;
    let store = state.store.clone();
    let crypto = state.crypto.clone();
    let notes = run_blocking(move || {
        let mut current = load_context_notes(&store, &crypto)?;
        for (message_id, note) in args.notes {
            let message_id = message_id.trim().to_string();
            if !message_id.is_empty() && !note.is_empty() {
                current.entry(message_id).or_insert(note);
            }
        }
        store_context_notes(&store, &crypto, &current)?;
        Ok(current)
    })
    .await?;
    serde_json::to_value(notes).map_err(ApiError::from_serialize)
}
