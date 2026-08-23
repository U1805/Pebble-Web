use std::collections::HashMap;

use pebble_core::{Folder, FolderRole, Message, PebbleError};
use pebble_store::Store;
use serde_json::{json, Value};

use super::folders::find_preferred_folder_by_role;

#[derive(Clone, Debug)]
enum DeleteDisposition {
    MoveToTrash {
        source: Option<Folder>,
        trash: Folder,
    },
    Permanent {
        source: Folder,
    },
    SoftDelete {
        source: Option<Folder>,
    },
}

/// Snapshot delete semantics before remote mutations change folder relations.
///
/// Upstream bulk-soft-deletes every successful batch item, including messages
/// that were just moved to Trash. That diverges from single-message delete and
/// makes the batch-deleted messages disappear from the Trash view.
pub(crate) struct BatchDeletePlan {
    dispositions: HashMap<String, DeleteDisposition>,
}

pub(crate) struct BatchDeleteFinalize {
    pub(crate) visible_ids: Vec<String>,
    pub(crate) removed_ids: Vec<String>,
}

impl BatchDeletePlan {
    pub(crate) fn capture(store: &Store, messages: &[Message]) -> Result<Self, PebbleError> {
        let mut dispositions = HashMap::with_capacity(messages.len());

        for message in messages {
            let folders = store.list_folders(&message.account_id)?;
            let folder_ids = store.get_message_folder_ids(&message.id)?;
            let source = folder_ids
                .iter()
                .find_map(|id| folders.iter().find(|folder| &folder.id == id));
            let trash =
                find_preferred_folder_by_role(store, &message.account_id, FolderRole::Trash)?;
            dispositions.insert(message.id.clone(), disposition_for(source, trash.as_ref()));
        }

        Ok(Self { dispositions })
    }

    pub(crate) fn is_permanent(&self, message_id: &str) -> bool {
        matches!(
            self.dispositions.get(message_id),
            Some(DeleteDisposition::Permanent { .. })
        )
    }

    pub(crate) fn pending_op_type(&self, message_id: &str) -> &'static str {
        if self.is_permanent(message_id) {
            "delete_permanent"
        } else {
            "delete"
        }
    }

    pub(crate) fn pending_payload(&self, message_id: &str) -> Value {
        match self.dispositions.get(message_id) {
            Some(DeleteDisposition::MoveToTrash { source, trash }) => json!({
                "trash": true,
                "source_folder_id": source.as_ref().map(|folder| folder.id.as_str()),
                "source_folder_remote_id": source.as_ref().map(|folder| folder.remote_id.as_str()),
                "trash_folder_id": trash.id,
                "trash_folder_remote_id": trash.remote_id,
                "permanent": false,
            }),
            Some(DeleteDisposition::Permanent { source }) => json!({
                "source_folder_id": source.id,
                "source_folder_remote_id": source.remote_id,
                "permanent": true,
            }),
            Some(DeleteDisposition::SoftDelete { source }) => json!({
                "trash": true,
                "source_folder_id": source.as_ref().map(|folder| folder.id.as_str()),
                "source_folder_remote_id": source.as_ref().map(|folder| folder.remote_id.as_str()),
                "permanent": false,
            }),
            None => json!({ "trash": true, "permanent": false }),
        }
    }

    pub(crate) fn finalize(
        &self,
        store: &Store,
        message_ids: &[String],
    ) -> Result<BatchDeleteFinalize, PebbleError> {
        let mut visible_ids = Vec::new();
        let mut removed_ids = Vec::new();

        for message_id in message_ids {
            match self.dispositions.get(message_id) {
                Some(DeleteDisposition::MoveToTrash { trash, .. }) => {
                    store.move_message_to_folder(message_id, &trash.id)?;
                    visible_ids.push(message_id.clone());
                }
                Some(DeleteDisposition::Permanent { .. }) => {
                    store.hard_delete_messages(std::slice::from_ref(message_id))?;
                    removed_ids.push(message_id.clone());
                }
                Some(DeleteDisposition::SoftDelete { .. }) | None => {
                    store.soft_delete_message(message_id)?;
                    removed_ids.push(message_id.clone());
                }
            }
        }

        Ok(BatchDeleteFinalize {
            visible_ids,
            removed_ids,
        })
    }
}

/// Finalize a replayed non-permanent delete without interpreting an existing
/// Trash relation as a request for permanent deletion. Permanent operations
/// already use the distinct `delete_permanent` pending-op type.
pub(crate) fn finalize_pending_trash(
    store: &Store,
    account_id: &str,
    message_id: &str,
    trash_folder_id: Option<String>,
) -> Result<(), PebbleError> {
    let trash_folder_id = match trash_folder_id {
        Some(folder_id) => Some(folder_id),
        None => find_preferred_folder_by_role(store, account_id, FolderRole::Trash)?
            .map(|folder| folder.id),
    };

    if let Some(folder_id) = trash_folder_id {
        // This is intentionally idempotent: move_message_to_folder also clears
        // a stale soft-delete flag left by an interrupted older implementation.
        store.move_message_to_folder(message_id, &folder_id)
    } else {
        store.soft_delete_message(message_id)
    }
}

fn disposition_for(source: Option<&Folder>, trash: Option<&Folder>) -> DeleteDisposition {
    if let Some(source) = source.filter(|folder| folder.role == Some(FolderRole::Trash)) {
        DeleteDisposition::Permanent {
            source: source.clone(),
        }
    } else if let Some(trash) = trash {
        DeleteDisposition::MoveToTrash {
            source: source.cloned(),
            trash: trash.clone(),
        }
    } else {
        DeleteDisposition::SoftDelete {
            source: source.cloned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pebble_core::{now_timestamp, Account, EmailAddress, FolderType, ProviderType};

    fn folder(id: &str, role: FolderRole) -> Folder {
        Folder {
            id: id.to_string(),
            account_id: "account-1".to_string(),
            remote_id: id.to_string(),
            name: id.to_string(),
            folder_type: FolderType::Folder,
            role: Some(role),
            parent_id: None,
            color: None,
            is_system: true,
            sort_order: 0,
        }
    }

    fn message() -> Message {
        let now = now_timestamp();
        Message {
            id: "message-1".to_string(),
            account_id: "account-1".to_string(),
            remote_id: "1".to_string(),
            message_id_header: Some("<message-1@example.test>".to_string()),
            in_reply_to: None,
            references_header: None,
            thread_id: None,
            subject: "subject".to_string(),
            snippet: String::new(),
            from_address: "sender@example.test".to_string(),
            from_name: "Sender".to_string(),
            to_list: vec![EmailAddress {
                name: None,
                address: "alice@example.test".to_string(),
            }],
            cc_list: Vec::new(),
            bcc_list: Vec::new(),
            body_text: String::new(),
            body_html_raw: String::new(),
            has_attachments: false,
            is_read: false,
            is_starred: false,
            is_draft: false,
            date: now,
            remote_version: None,
            is_deleted: false,
            deleted_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn regular_delete_moves_to_trash_instead_of_soft_deleting() {
        let inbox = folder("inbox", FolderRole::Inbox);
        let trash = folder("trash", FolderRole::Trash);

        assert!(matches!(
            disposition_for(Some(&inbox), Some(&trash)),
            DeleteDisposition::MoveToTrash { trash: folder, .. } if folder.id == "trash"
        ));
    }

    #[test]
    fn deleting_from_trash_is_permanent() {
        let trash = folder("trash", FolderRole::Trash);

        assert!(matches!(
            disposition_for(Some(&trash), Some(&trash)),
            DeleteDisposition::Permanent { .. }
        ));
    }

    #[test]
    fn missing_trash_falls_back_to_soft_delete() {
        let inbox = folder("inbox", FolderRole::Inbox);

        assert!(matches!(
            disposition_for(Some(&inbox), None),
            DeleteDisposition::SoftDelete { .. }
        ));
    }

    #[test]
    fn replayed_regular_delete_stays_visible_when_already_in_trash() {
        let store = Store::open_in_memory().unwrap();
        let now = now_timestamp();
        store
            .insert_account(&Account {
                id: "account-1".to_string(),
                email: "alice@example.test".to_string(),
                display_name: "Alice".to_string(),
                color: None,
                provider: ProviderType::Imap,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        let trash = folder("trash", FolderRole::Trash);
        store.insert_folder(&trash).unwrap();
        store
            .insert_message(&message(), std::slice::from_ref(&trash.id))
            .unwrap();
        store.soft_delete_message("message-1").unwrap();

        finalize_pending_trash(&store, "account-1", "message-1", Some(trash.id.clone())).unwrap();

        let restored = store.get_message("message-1").unwrap().unwrap();
        assert!(!restored.is_deleted);
        assert_eq!(
            store.get_message_folder_ids("message-1").unwrap(),
            vec![trash.id]
        );
    }
}
