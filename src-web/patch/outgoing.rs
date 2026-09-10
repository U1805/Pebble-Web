use pebble_core::{Message, PebbleError};
use pebble_mail::thread::compute_thread_id;
use pebble_store::Store;
use std::collections::HashSet;

/// Assign a thread before an outgoing placeholder is stored. Upstream leaves
/// local Sent messages with a NULL thread_id, which hides the entire Sent
/// folder in conversation view and prevents replies joining their parent.
pub(crate) fn assign_thread_id(store: &Store, message: &mut Message) -> Result<(), PebbleError> {
    let mut reference_ids = HashSet::new();
    for header in [&message.in_reply_to, &message.references_header]
        .into_iter()
        .flatten()
    {
        reference_ids.extend(header.split_whitespace().map(str::to_string));
    }
    let reference_ids = reference_ids.into_iter().collect::<Vec<_>>();
    let mappings = store.get_thread_mappings_for_refs(&message.account_id, &reference_ids)?;
    message.thread_id = Some(compute_thread_id(message, &mappings));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pebble_core::{new_id, now_timestamp, Account, EmailAddress, ProviderType};

    fn message(account_id: &str, message_id: &str) -> Message {
        let now = now_timestamp();
        Message {
            id: new_id(),
            account_id: account_id.to_string(),
            remote_id: new_id(),
            message_id_header: Some(message_id.to_string()),
            in_reply_to: None,
            references_header: None,
            thread_id: None,
            subject: "subject".to_string(),
            snippet: String::new(),
            from_address: "alice@example.com".to_string(),
            from_name: "Alice".to_string(),
            to_list: vec![EmailAddress {
                name: None,
                address: "bob@example.com".to_string(),
            }],
            cc_list: Vec::new(),
            bcc_list: Vec::new(),
            body_text: String::new(),
            body_html_raw: String::new(),
            has_attachments: false,
            is_read: true,
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
    fn roots_new_messages_and_joins_replies_to_existing_thread() {
        let store = Store::open_in_memory().unwrap();
        let now = now_timestamp();
        store
            .insert_account(&Account {
                account_label: None,
                provider_display_name: None,
                id: "account-1".to_string(),
                email: "alice@example.com".to_string(),
                display_name: "Alice".to_string(),
                color: None,
                provider: ProviderType::Imap,
                created_at: now,
                updated_at: now,
            })
            .unwrap();

        let mut parent = message("account-1", "<parent@example.com>");
        assign_thread_id(&store, &mut parent).unwrap();
        store.insert_message(&parent, &[]).unwrap();

        let mut reply = message("account-1", "<reply@example.com>");
        reply.in_reply_to = parent.message_id_header.clone();
        reply.references_header = parent.message_id_header.clone();
        assign_thread_id(&store, &mut reply).unwrap();

        assert_eq!(parent.thread_id.as_deref(), Some("<parent@example.com>"));
        assert_eq!(reply.thread_id, parent.thread_id);
    }
}
