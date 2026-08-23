//! Search indexing + rule-application pipeline.
//!
//! Mirrors the responsibility of `src-tauri/src/commands/indexing.rs`: newly
//! stored messages pass through rule actions and then enter the search index.

use std::sync::Arc;

use pebble_core::{FolderRole, KanbanCard, Message, PebbleError};
use pebble_rules::{types::RuleAction, RuleEngine};
use pebble_search::TantivySearch;
use pebble_store::Store;
use serde_json::json;
use tracing::{info, warn};



/// Replay crash-recovery search operations before long-lived mail workers
/// start. Markers are only cleared after a successful Tantivy commit.
pub(crate) fn recover_pending_search_operations(
    store: &Store,
    search: &TantivySearch,
) -> std::result::Result<usize, PebbleError> {
    let pending = store.list_search_pending()?;
    let mut applied_ids = Vec::with_capacity(pending.len());
    for (message_id, operation) in pending {
        let result = if operation == "remove" {
            search.remove_message(&message_id)
        } else {
            match store.get_message(&message_id) {
                Ok(Some(message)) if !message.is_deleted => {
                    match store.get_message_folder_ids(&message_id) {
                        Ok(folder_ids) if !folder_ids.is_empty() => {
                            search.index_message(&message, &folder_ids)
                        }
                        Ok(_) => search.remove_message(&message_id),
                        Err(error) => Err(error),
                    }
                }
                Ok(_) => search.remove_message(&message_id),
                Err(error) => Err(error),
            }
        };
        match result {
            Ok(()) => applied_ids.push(message_id),
            Err(error) => warn!(
                "Failed to recover search operation for message {}: {}",
                message_id, error
            ),
        }
    }

    if applied_ids.is_empty() {
        return Ok(0);
    }
    search.commit()?;
    store.clear_search_pending(&applied_ids)?;
    Ok(applied_ids.len())
}

pub(crate) fn new_mail_event_payload(stored: &pebble_mail::StoredMessage) -> serde_json::Value {
    serde_json::json!({
        "account_id": stored.message.account_id,
        "message_id": stored.message.id,
        "folder_ids": stored.folder_ids,
        "thread_id": stored.message.thread_id,
        "subject": stored.message.subject,
        "from": stored.message.from_address,
        "received_at": stored.message.date,
    })
}

/// Rebuild the search index from all messages currently in the store.
pub(crate) fn new_mail_notification_body(stored: &pebble_mail::StoredMessage) -> String {
    let sender = if stored.message.from_name.trim().is_empty() {
        stored.message.from_address.trim()
    } else {
        stored.message.from_name.trim()
    };
    let subject = stored.message.subject.trim();

    match (sender.is_empty(), subject.is_empty()) {
        (true, true) => "New message".to_string(),
        (true, false) => subject.to_string(),
        (false, true) => sender.to_string(),
        (false, false) => format!("{sender}: {subject}"),
    }
}

pub(crate) fn should_notify_new_mail(
    store: &Store,
    stored: &pebble_mail::StoredMessage,
) -> Result<bool, PebbleError> {
    use std::collections::HashSet;

    if !stored.notify || stored.message.is_deleted || stored.message.is_draft {
        return Ok(false);
    }

    let folder_ids: HashSet<&str> = stored.folder_ids.iter().map(String::as_str).collect();
    if folder_ids.is_empty() {
        return Ok(false);
    }

    let folders = store.list_folders(&stored.message.account_id)?;
    Ok(folders.iter().any(|folder| {
        folder.role == Some(FolderRole::Inbox) && folder_ids.contains(folder.id.as_str())
    }))
}

pub fn do_reindex(store: &Store, search: &TantivySearch) -> std::result::Result<u32, PebbleError> {
    search.clear_index()?;

    let accounts = store.list_accounts()?;
    let mut count: u32 = 0;
    let batch_size = 200u32;

    for account in &accounts {
        let mut offset = 0u32;
        loop {
            let messages = store.list_full_messages_by_account(&account.id, batch_size, offset)?;
            if messages.is_empty() {
                break;
            }

            let ids: Vec<String> = messages.iter().map(|m| m.id.clone()).collect();
            let folder_map = store.get_message_folder_ids_batch(&ids)?;

            let batch: Vec<_> = messages
                .iter()
                .map(|msg| {
                    let folder_ids = folder_map.get(&msg.id).cloned().unwrap_or_default();
                    (msg.clone(), folder_ids)
                })
                .collect();
            let batch_len = batch.len() as u32;
            if let Err(e) = search.index_messages_batch(&batch) {
                warn!("Failed to index batch of {} messages: {}", batch_len, e);
            } else {
                count += batch_len;
            }

            offset += messages.len() as u32;
            if (messages.len() as u32) < batch_size {
                break;
            }
        }
    }

    search.commit()?;
    info!("Reindexed {} messages", count);
    Ok(count)
}

fn apply_web_rule_action(
    store: &Store,
    message: &Message,
    action: &RuleAction,
) -> Result<(), PebbleError> {
    match action {
        RuleAction::MarkRead => {
            if queue_remote_rule_action(store, &message.account_id, &message.id, action)? {
                info!("Rule: queued remote mark-read for message {}", message.id);
                return Ok(());
            }
            store.update_message_flags(&message.id, Some(true), None)?;
            info!("Rule: marked message {} as read", message.id);
        }
        RuleAction::Archive => {
            if queue_remote_rule_action(store, &message.account_id, &message.id, action)? {
                info!("Rule: queued remote archive for message {}", message.id);
                return Ok(());
            }
            if let Some(archive_folder) = crate::patch::folders::find_preferred_folder_by_role(
                store,
                &message.account_id,
                FolderRole::Archive,
            )? {
                store.move_message_to_folder(&message.id, &archive_folder.id)?;
                info!(
                    "Rule: archived message {} to folder {}",
                    message.id, archive_folder.name
                );
            } else {
                store.soft_delete_message(&message.id)?;
                info!(
                    "Rule: archived (soft-deleted) message {} (no archive folder)",
                    message.id
                );
            }
        }
        RuleAction::AddLabel(label) => {
            store.add_label(&message.id, label)?;
            info!("Rule: added label '{}' to message {}", label, message.id);
        }
        RuleAction::MoveToFolder(folder_name) => {
            if queue_remote_rule_action(store, &message.account_id, &message.id, action)? {
                info!(
                    "Rule: queued remote move for message {} to folder '{}'",
                    message.id, folder_name
                );
                return Ok(());
            }
            if let Some(target_folder) =
                store.find_folder_by_name(&message.account_id, folder_name)?
            {
                store.move_message_to_folder(&message.id, &target_folder.id)?;
                info!(
                    "Rule: moved message {} to folder '{}'",
                    message.id, target_folder.name
                );
            } else {
                warn!(
                    "Rule: target folder '{}' not found for account {}",
                    folder_name, message.account_id
                );
            }
        }
        RuleAction::SetKanbanColumn(column) => {
            let now = pebble_core::now_timestamp();
            let card = KanbanCard {
                message_id: message.id.clone(),
                column: column.clone(),
                position: 0,
                created_at: now,
                updated_at: now,
            };
            store.upsert_kanban_card(&card)?;
            info!(
                "Rule: added message {} to kanban column {:?}",
                message.id, column
            );
        }
    }
    Ok(())
}

fn queue_remote_rule_action(
    store: &Store,
    account_id: &str,
    message_id: &str,
    action: &RuleAction,
) -> Result<bool, PebbleError> {
    use crate::commands::gmail_labels::gmail_move_label_delta;
    use pebble_core::ProviderType;

    let Some(account) = store.get_account(account_id)? else {
        return Ok(false);
    };
    let Some(message) = store.get_message(message_id)? else {
        return Ok(false);
    };
    if account.provider == ProviderType::Pop3 {
        return Ok(false);
    }
    let source_folder = store
        .get_message_folder_ids(message_id)?
        .into_iter()
        .next()
        .and_then(|folder_id| {
            store
                .list_folders(account_id)
                .ok()?
                .into_iter()
                .find(|folder| folder.id == folder_id)
        });

    match action {
        RuleAction::MarkRead => {
            if account.provider == ProviderType::Imap
                && source_folder
                    .as_ref()
                    .is_some_and(|folder| folder.remote_id.starts_with("__local_"))
            {
                return Ok(false);
            }

            let mut payload = json!({
                "is_read": true,
                "is_starred": null,
            });
            if account.provider == ProviderType::Gmail {
                payload["add_labels"] = json!([]);
                payload["remove_labels"] = json!(["UNREAD"]);
            }
            if let Some(folder) = source_folder.as_ref() {
                payload["folder_remote_id"] = json!(folder.remote_id);
            }
            crate::commands::pending_mail_ops::queue_pending_for_store(
                store,
                &message,
                "update_flags",
                payload,
            )?;
            Ok(true)
        }
        RuleAction::Archive => {
            let archive_folder = crate::patch::folders::find_preferred_folder_by_role(
                store,
                account_id,
                FolderRole::Archive,
            )?;
            if let Some(archive) = archive_folder.as_ref() {
                if archive.remote_id.starts_with("__local_") {
                    return Ok(false);
                }
            } else if account.provider != ProviderType::Gmail {
                return Ok(false);
            }

            let mut payload = json!({
                "source_folder_id": source_folder.as_ref().map(|folder| folder.id.as_str()),
                "source_folder_remote_id": source_folder.as_ref().map(|folder| folder.remote_id.as_str()),
                "target_folder_id": archive_folder.as_ref().map(|folder| folder.id.as_str()),
                "target_folder_remote_id": archive_folder.as_ref().map(|folder| folder.remote_id.as_str()),
            });
            if account.provider == ProviderType::Gmail {
                payload["add_labels"] = json!([]);
                payload["remove_labels"] = json!(["INBOX"]);
            }
            crate::commands::pending_mail_ops::queue_pending_for_store(
                store,
                &message,
                "archive",
                payload,
            )?;
            Ok(true)
        }
        RuleAction::MoveToFolder(folder_name) => {
            let Some(target_folder) = store.find_folder_by_name(account_id, folder_name)? else {
                return Ok(false);
            };
            if target_folder.remote_id.starts_with("__local_") {
                return Ok(false);
            }

            let mut payload = json!({
                "source_folder_id": source_folder.as_ref().map(|folder| folder.id.as_str()),
                "source_folder_remote_id": source_folder.as_ref().map(|folder| folder.remote_id.as_str()),
                "target_folder_id": target_folder.id.as_str(),
                "target_folder_remote_id": target_folder.remote_id.as_str(),
            });
            if account.provider == ProviderType::Gmail {
                let delta = gmail_move_label_delta(
                    source_folder
                        .as_ref()
                        .map(|folder| folder.remote_id.as_str()),
                    &target_folder.remote_id,
                    target_folder.role,
                );
                payload["add_labels"] = json!(delta.add_labels);
                payload["remove_labels"] = json!(delta.remove_labels);
            }
            crate::commands::pending_mail_ops::queue_pending_for_store(
                store,
                &message,
                "move_to_folder",
                payload,
            )?;
            Ok(true)
        }
        RuleAction::AddLabel(_) | RuleAction::SetKanbanColumn(_) => Ok(false),
    }
}

fn apply_web_rules(store: &Store, message: &Message) -> Result<(), PebbleError> {
    let rules = store.list_rules()?;
    let engine = RuleEngine::new(&rules);
    for action in engine.evaluate(message) {
        if let Err(error) = apply_web_rule_action(store, message, &action) {
            warn!(message_id = %message.id, "Rule action failed: {error}");
        }
    }
    Ok(())
}

/// Apply rules to a newly stored message and refresh its search document.
///
/// The final search operation is derived from the latest store state, matching
/// the Tauri indexing pipeline: deleted, missing, or folderless messages remove
/// any stale document instead of being indexed. The recovery marker remains
/// durable until the Tantivy commit succeeds.
pub(crate) async fn index_stored_message(
    search: &Arc<TantivySearch>,
    store: &Arc<Store>,
    stored: &pebble_mail::StoredMessage,
) {
    let search = search.clone();
    let store = store.clone();
    let message_id = stored.message.id.clone();
    let rule_message = stored.message.clone();
    let stored_reconciliation = stored.reconciliation;
    let apply_rules = !stored_reconciliation;

    let outcome = tokio::task::spawn_blocking(move || -> Result<(), PebbleError> {
        if apply_rules {
            if let Err(error) = apply_web_rules(&store, &rule_message) {
                warn!(message_id = %message_id, "Failed to apply Web rules: {error}");
            }
        }

        let ids = vec![message_id.clone()];

        match store.get_message(&message_id)? {
            Some(message) if !message.is_deleted => {
                let folder_ids = store.get_message_folder_ids(&message_id)?;
                if folder_ids.is_empty() {
                    search.remove_message(&message_id)?;
                } else {
                    search.index_message(&message, &folder_ids)?;
                }
            }
            Some(_) | None => {
                search.remove_message(&message_id)?;
            }
        }

        search.commit()?;
        if stored_reconciliation {
            store.clear_search_pending(&ids)?;
        }
        Ok(())
    })
    .await;

    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            warn!(message_id = %stored.message.id, "Failed to refresh synced search document: {error}");
        }
        Err(error) => {
            warn!(message_id = %stored.message.id, "Search indexing task failed: {error}");
        }
    }
}
