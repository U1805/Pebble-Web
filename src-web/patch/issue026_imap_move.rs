use std::collections::HashSet;

use pebble_core::{Message, PebbleError};
use pebble_mail::ImapProvider;
use pebble_store::Store;

/// Extend QingJ01/Pebble issue #26's remote-identity fix to IMAP moves.
///
/// Move an IMAP message and keep the stored mailbox-scoped UID in sync.
///
/// IMAP MOVE commonly assigns a new UID in the destination mailbox. Upstream
/// moves the local row to that mailbox while retaining the source UID, so the
/// next sync imports the destination copy as a second active message.
pub(crate) async fn move_message_and_update_uid(
    imap: &ImapProvider,
    store: &Store,
    message: &Message,
    source_mailbox: &str,
    destination_mailbox: &str,
) -> Result<(), PebbleError> {
    let source_uid = message
        .remote_id
        .parse::<u32>()
        .map_err(|error| PebbleError::Internal(format!("Invalid IMAP UID: {error}")))?;
    let before_uids: HashSet<u32> = imap
        .fetch_all_uids(destination_mailbox)
        .await?
        .into_iter()
        .collect();

    imap.move_message_with_dedup(
        source_mailbox,
        source_uid,
        destination_mailbox,
        message.message_id_header.as_deref(),
    )
    .await?;

    let destination_exists = imap.select_exists(destination_mailbox).await?;
    let start = destination_exists.saturating_sub(49).max(1);
    let recent = imap
        .fetch_messages_page(destination_mailbox, start, destination_exists)
        .await?;
    let new_uid =
        select_destination_uid(&recent, &before_uids, message.message_id_header.as_deref());

    let new_uid = new_uid.ok_or_else(|| {
        PebbleError::Sync(format!(
            "IMAP move succeeded but the destination UID could not be resolved for {}",
            message.id
        ))
    })?;
    let destination = store
        .list_folders(&message.account_id)?
        .into_iter()
        .find(|folder| folder.remote_id == destination_mailbox)
        .ok_or_else(|| {
            PebbleError::Internal(format!(
                "No stored destination folder for IMAP mailbox {destination_mailbox}"
            ))
        })?;

    // A destination UID may legitimately equal another UID in the source
    // mailbox. Clear the mailbox-scoped identity before changing the folder,
    // then install the destination UID. The surrounding command's normal
    // local move becomes an idempotent second application.
    let temporary_remote_id = format!("__web_imap_move_{}__", message.id);
    store.update_remote_id(&message.id, &temporary_remote_id)?;
    if let Err(error) = store.move_message_to_folder(&message.id, &destination.id) {
        let _ = store.update_remote_id(&message.id, &message.remote_id);
        return Err(error);
    }
    if let Err(error) = store.update_remote_id(&message.id, &new_uid.to_string()) {
        // A concurrent sync may already have imported the authoritative target
        // row. Keep only that row visible rather than exposing two messages.
        let _ = store.soft_delete_message(&message.id);
        return Err(error);
    }

    Ok(())
}

fn select_destination_uid(
    recent: &[(u32, Vec<u8>)],
    before_uids: &HashSet<u32>,
    message_id_header: Option<&str>,
) -> Option<u32> {
    if let Some(message_id_header) = message_id_header.filter(|value| !value.is_empty()) {
        if let Some(uid) = recent
            .iter()
            .filter_map(|(uid, raw)| {
                let parsed = pebble_mail::parser::parse_raw_email(raw).ok()?;
                (parsed.message_id_header.as_deref() == Some(message_id_header)).then_some(*uid)
            })
            .max()
        {
            return Some(uid);
        }
    }

    let mut new_uids = recent
        .iter()
        .map(|(uid, _)| *uid)
        .filter(|uid| !before_uids.contains(uid));
    let only = new_uids.next()?;
    new_uids.next().is_none().then_some(only)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(message_id: &str) -> Vec<u8> {
        format!("Message-ID: {message_id}\r\nSubject: test\r\n\r\nbody").into_bytes()
    }

    #[test]
    fn selects_matching_message_id_over_unrelated_new_mail() {
        let recent = vec![
            (8, raw("<other@example.test>")),
            (9, raw("<target@example.test>")),
        ];
        let before = HashSet::from([1, 2, 3]);

        assert_eq!(
            select_destination_uid(&recent, &before, Some("<target@example.test>")),
            Some(9)
        );
    }

    #[test]
    fn uses_the_only_new_uid_when_message_id_is_missing() {
        let recent = vec![
            (7, raw("<old@example.test>")),
            (8, raw("<new@example.test>")),
        ];
        let before = HashSet::from([7]);

        assert_eq!(select_destination_uid(&recent, &before, None), Some(8));
    }

    #[test]
    fn refuses_ambiguous_uid_fallback() {
        let recent = vec![
            (8, raw("<one@example.test>")),
            (9, raw("<two@example.test>")),
        ];
        let before = HashSet::new();

        assert_eq!(select_destination_uid(&recent, &before, None), None);
    }
}
