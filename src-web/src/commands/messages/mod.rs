pub mod flags;
pub mod lifecycle;
pub mod query;
pub mod rendering;

pub(crate) use query::{get_message, get_messages_batch, list_messages, list_starred_messages};
pub(crate) use rendering::{get_message_with_html, get_rendered_html, is_trusted_sender};

// Shared helpers mirror `src-tauri/src/commands/messages/mod.rs` so provider
// configuration and search-index maintenance stay easy to compare with upstream.

use crate::commands::encrypted_store::load_account_auth_data;
use crate::commands::network::{
    account_proxy_mode_from_auth_value, resolve_mail_proxy_from_mode, AccountProxyMode,
};
use crate::state::AppState;
use pebble_core::PebbleError;
use pebble_crypto::CryptoService;
use pebble_mail::{ImapConfig, Pop3Config};
use pebble_store::Store;

pub(crate) fn refresh_search_documents(
    state: &AppState,
    message_ids: &[String],
) -> std::result::Result<(), PebbleError> {
    if message_ids.is_empty() { return Ok(()); }
    state.store.add_search_pending(message_ids, "index")?;
    for message_id in message_ids {
        match state.store.get_message(message_id)? {
            Some(message) if !message.is_deleted => {
                let folder_ids = state.store.get_message_folder_ids(message_id)?;
                if folder_ids.is_empty() {
                    state.search.remove_message(message_id)?;
                } else {
                    state.search.index_message(&message, &folder_ids)?;
                }
            }
            Some(_) | None => state.search.remove_message(message_id)?,
        }
    }
    state.search.commit()?;
    state.store.clear_search_pending(message_ids)?;
    Ok(())
}

pub(crate) fn load_imap_config(
    store: &Store,
    crypto: &CryptoService,
    account_id: &str,
) -> std::result::Result<ImapConfig, PebbleError> {
    let (mut config, proxy_mode): (ImapConfig, AccountProxyMode) = if let Some(decrypted) =
        load_account_auth_data(crypto, store, account_id)?
    {
        let value: serde_json::Value = serde_json::from_slice(&decrypted)
            .map_err(|e| PebbleError::Internal(format!("Failed to parse config: {e}")))?;
        let proxy_mode = account_proxy_mode_from_auth_value(&value);
        let config = serde_json::from_value(value.get("imap").cloned().unwrap_or(value.clone()))
            .map_err(|e| PebbleError::Internal(format!("Failed to deserialize IMAP config: {e}")))?;
        (config, proxy_mode)
    } else {
        let sync_state = store
            .get_sync_state(account_id)?
            .ok_or_else(|| PebbleError::Internal(format!("No config for account {account_id}")))?;
        let imap_value = sync_state.imap.ok_or_else(|| {
            PebbleError::Internal(format!("No IMAP config for account {account_id}"))
        })?;
        let config = serde_json::from_value(imap_value)
            .map_err(|e| PebbleError::Internal(format!("Failed to deserialize IMAP config: {e}")))?;
        (config, AccountProxyMode::Inherit)
    };
    config.proxy = resolve_mail_proxy_from_mode(crypto, store, proxy_mode, config.proxy)?;
    Ok(config)
}

pub(crate) fn load_pop3_config(
    store: &Store,
    crypto: &CryptoService,
    account_id: &str,
) -> std::result::Result<Pop3Config, PebbleError> {
    let (imap_config, proxy_mode) = if let Some(decrypted) =
        load_account_auth_data(crypto, store, account_id)?
    {
        let value: serde_json::Value = serde_json::from_slice(&decrypted)
            .map_err(|e| PebbleError::Internal(format!("Failed to parse config: {e}")))?;
        let proxy_mode = account_proxy_mode_from_auth_value(&value);
        let config: ImapConfig = serde_json::from_value(
            value.get("imap").cloned().unwrap_or(value.clone()),
        )
        .map_err(|e| PebbleError::Internal(format!("Failed to deserialize POP3 config: {e}")))?;
        (config, proxy_mode)
    } else {
        return Err(PebbleError::Internal(format!("No POP3 config for account {account_id}")));
    };
    let proxy = resolve_mail_proxy_from_mode(crypto, store, proxy_mode, imap_config.proxy)?;
    Ok(Pop3Config {
        host: imap_config.host,
        port: imap_config.port,
        username: imap_config.username,
        password: imap_config.password,
        security: imap_config.security,
        accept_invalid_certs: imap_config.accept_invalid_certs,
        proxy,
    })
}
