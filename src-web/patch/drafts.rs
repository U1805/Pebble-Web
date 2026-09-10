use pebble_core::{new_id, Folder, FolderRole, FolderType, PebbleError};
use pebble_store::Store;

/// Return a usable Drafts folder, creating a local-only fallback when the
/// provider did not expose one. Saving a draft without any folder relation
/// makes it unreachable after the compose view is reloaded.
pub(crate) fn ensure_local_drafts_folder(
    store: &Store,
    account_id: &str,
) -> Result<Folder, PebbleError> {
    if let Some(folder) =
        crate::patch::folders::find_preferred_folder_by_role(store, account_id, FolderRole::Drafts)?
    {
        return Ok(folder);
    }

    if let Some(folder) = store.find_folder_by_name(account_id, "Drafts")? {
        return Ok(folder);
    }

    let folder = Folder {
        id: new_id(),
        account_id: account_id.to_string(),
        remote_id: "__local_drafts__".to_string(),
        name: "Drafts".to_string(),
        folder_type: FolderType::Folder,
        role: Some(FolderRole::Drafts),
        parent_id: None,
        color: None,
        is_system: true,
        sort_order: 1,
    };
    let id = store.insert_folder(&folder)?;
    Ok(Folder { id, ..folder })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pebble_core::{now_timestamp, Account, ProviderType};

    #[test]
    fn creates_and_reuses_local_drafts_folder() {
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

        let first = ensure_local_drafts_folder(&store, "account-1").unwrap();
        let second = ensure_local_drafts_folder(&store, "account-1").unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(first.remote_id, "__local_drafts__");
        assert_eq!(first.role, Some(FolderRole::Drafts));
    }
}
