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


/// Rebuild the search index from all messages currently in the store.
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
    let source_folder = || -> Result<Option<pebble_core::Folder>, PebbleError> {
        let folder_ids = store.get_message_folder_ids(&message.id)?;
        let folders = store.list_folders(&message.account_id)?;
        Ok(folder_ids
            .iter()
            .find_map(|id| folders.iter().find(|folder| &folder.id == id).cloned()))
    };

    match action {
        RuleAction::MarkRead => {
            store.update_message_flags(&message.id, Some(true), None)?;
            crate::commands::pending_mail_ops::queue_pending_for_store(
                store,
                message,
                "update_flags",
                json!({ "is_read": true, "is_starred": null }),
            )?;
        }
        RuleAction::Archive => {
            let source = source_folder()?;
            if let Some(archive) =
                store.find_folder_by_role(&message.account_id, FolderRole::Archive)?
            {
                store.move_message_to_folder(&message.id, &archive.id)?;
                crate::commands::pending_mail_ops::queue_pending_for_store(
                    store,
                    message,
                    "archive",
                    json!({
                        "source_folder_id": source.as_ref().map(|folder| folder.id.as_str()),
                        "source_folder_remote_id": source.as_ref().map(|folder| folder.remote_id.as_str()),
                        "target_folder_id": archive.id,
                        "target_folder_remote_id": archive.remote_id,
                    }),
                )?;
            } else {
                store.soft_delete_message(&message.id)?;
                crate::commands::pending_mail_ops::queue_pending_for_store(
                    store,
                    message,
                    "archive",
                    json!({
                        "source_folder_id": source.as_ref().map(|folder| folder.id.as_str()),
                        "source_folder_remote_id": source.as_ref().map(|folder| folder.remote_id.as_str()),
                        "trash_or_soft_delete": true,
                    }),
                )?;
            }
        }
        RuleAction::AddLabel(label) => {
            store.add_label(&message.id, label)?;
        }
        RuleAction::MoveToFolder(folder_name) => {
            let Some(target) = store.find_folder_by_name(&message.account_id, folder_name)? else {
                warn!(
                    message_id = %message.id,
                    account_id = %message.account_id,
                    folder = %folder_name,
                    "Rule target folder not found"
                );
                return Ok(());
            };
            let source = source_folder()?;
            store.move_message_to_folder(&message.id, &target.id)?;
            crate::commands::pending_mail_ops::queue_pending_for_store(
                store,
                message,
                "move_to_folder",
                json!({
                    "source_folder_id": source.as_ref().map(|folder| folder.id.as_str()),
                    "source_folder_remote_id": source.as_ref().map(|folder| folder.remote_id.as_str()),
                    "target_folder_id": target.id,
                    "target_folder_remote_id": target.remote_id,
                }),
            )?;
        }
        RuleAction::SetKanbanColumn(column) => {
            let now = pebble_core::now_timestamp();
            store.upsert_kanban_card(&KanbanCard {
                message_id: message.id.clone(),
                column: column.clone(),
                position: 0,
                created_at: now,
                updated_at: now,
            })?;
        }
    }
    Ok(())
}

fn apply_web_rules(store: &Store, message: &Message) -> Result<(), PebbleError> {
    let rules = store.list_rules()?;
    let engine = RuleEngine::new(&rules);
    for action in engine.evaluate(message) {
        apply_web_rule_action(store, message, &action)?;
    }
    Ok(())
}

/// 新同步消息执行规则并入搜索索引（逐条 add + index + commit）。
pub(crate) async fn index_stored_message(
    search: &Arc<TantivySearch>,
    store: &Arc<Store>,
    stored: &pebble_mail::StoredMessage,
) {
    let search = search.clone();
    let store = store.clone();
    let mut message = stored.message.clone();
    let mut folder_ids = stored.folder_ids.clone();
    let apply_rules = !stored.reconciliation;
    let outcome = tokio::task::spawn_blocking(move || -> Result<(), PebbleError> {
        if apply_rules {
            if let Err(error) = apply_web_rules(&store, &message) {
                warn!(message_id = %message.id, "Failed to apply Web rules: {error}");
            }
            if let Some(latest) = store.get_message(&message.id)? {
                message = latest;
                folder_ids = store.get_message_folder_ids(&message.id)?;
            }
        }
        let ids = vec![message.id.clone()];
        store.add_search_pending(&ids, "index")?;
        search.index_message(&message, &folder_ids)?;
        search.commit()?;
        store.clear_search_pending(&ids)?;
        Ok(())
    })
    .await;
    if let Err(e) = outcome {
        warn!("Failed to index synced message: {:?}", e);
    }
}
