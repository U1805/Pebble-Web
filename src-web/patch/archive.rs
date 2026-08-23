use pebble_core::{Folder, PebbleError, ProviderType};

/// IMAP UIDs are mailbox-scoped. Moving an INBOX row only to a local Archive
/// folder while leaving the server copy in INBOX causes the next sync to
/// import a second active row. Reject that unsafe fallback until a real
/// server-side Archive mailbox is available.
pub(crate) fn reject_unsafe_imap_local_archive(
    provider: &ProviderType,
    archive: &Folder,
) -> Result<(), PebbleError> {
    if matches!(provider, ProviderType::Imap) && archive.remote_id.starts_with("__local_") {
        return Err(PebbleError::UnsupportedProvider(
            "This IMAP account has no server-side Archive folder".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pebble_core::{FolderRole, FolderType};

    fn archive(remote_id: &str) -> Folder {
        Folder {
            id: "archive".to_string(),
            account_id: "account-1".to_string(),
            remote_id: remote_id.to_string(),
            name: "Archive".to_string(),
            folder_type: FolderType::Folder,
            role: Some(FolderRole::Archive),
            parent_id: None,
            color: None,
            is_system: true,
            sort_order: 3,
        }
    }

    #[test]
    fn rejects_only_local_imap_archive_targets() {
        assert!(reject_unsafe_imap_local_archive(
            &ProviderType::Imap,
            &archive("__local_archive__")
        )
        .is_err());
        assert!(reject_unsafe_imap_local_archive(&ProviderType::Imap, &archive("Archive")).is_ok());
        assert!(reject_unsafe_imap_local_archive(
            &ProviderType::Pop3,
            &archive("__local_archive__")
        )
        .is_ok());
    }
}
