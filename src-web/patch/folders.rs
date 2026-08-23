use pebble_core::{Folder, FolderRole, PebbleError};
use pebble_store::Store;

/// Prefer a provider-backed system folder over its local fallback.
///
/// Upstream keeps `__local_*` folders when a remote folder later appears.
/// A role lookup without an explicit order can consequently keep returning
/// the fallback and prevent remote mutations from using the available server
/// folder.
pub(crate) fn find_preferred_folder_by_role(
    store: &Store,
    account_id: &str,
    role: FolderRole,
) -> Result<Option<Folder>, PebbleError> {
    let folders = store.list_folders(account_id)?;
    Ok(preferred_folder_by_role(&folders, role).cloned())
}

fn preferred_folder_by_role(folders: &[Folder], role: FolderRole) -> Option<&Folder> {
    folders
        .iter()
        .filter(|folder| folder.role == Some(role.clone()))
        .min_by_key(|folder| folder.remote_id.starts_with("__local_"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pebble_core::FolderType;

    fn folder(id: &str, remote_id: &str, role: FolderRole) -> Folder {
        Folder {
            id: id.to_string(),
            account_id: "account-1".to_string(),
            remote_id: remote_id.to_string(),
            name: "Archive".to_string(),
            folder_type: FolderType::Folder,
            role: Some(role),
            parent_id: None,
            color: None,
            is_system: true,
            sort_order: 3,
        }
    }

    #[test]
    fn prefers_remote_folder_over_earlier_local_fallback() {
        let folders = vec![
            folder("local", "__local_archive__", FolderRole::Archive),
            folder("remote", "Archive", FolderRole::Archive),
        ];

        let selected = preferred_folder_by_role(&folders, FolderRole::Archive).unwrap();

        assert_eq!(selected.id, "remote");
    }

    #[test]
    fn keeps_local_fallback_when_no_remote_folder_exists() {
        let folders = vec![folder("local", "__local_archive__", FolderRole::Archive)];

        let selected = preferred_folder_by_role(&folders, FolderRole::Archive).unwrap();

        assert_eq!(selected.id, "local");
    }
}
