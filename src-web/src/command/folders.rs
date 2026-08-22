use pebble_core::traits::FolderProvider;
use pebble_core::{new_id, Folder, FolderRole, FolderType, ProviderType};
use pebble_mail::{should_hide_outlook_folder, ImapMailProvider};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

#[derive(Debug, Clone, Serialize)]
pub struct ImapSyncFolderSettings {
    pub folders: Vec<Folder>,
    pub selected_remote_ids: Vec<String>,
}

fn provider_folders_have_arrived(folders: &[Folder]) -> bool {
    folders
        .iter()
        .any(|folder| !folder.remote_id.starts_with("__local_"))
}

fn should_seed_local_archive(folders: &[Folder]) -> bool {
    let has_archive = folders
        .iter()
        .any(|folder| folder.role == Some(FolderRole::Archive));
    provider_folders_have_arrived(folders) && !has_archive
}

fn should_hide_stored_outlook_folder(folder: &Folder) -> bool {
    folder.role.is_none()
        && !folder.remote_id.starts_with("__local_")
        && should_hide_outlook_folder(Some(&folder.name), None)
}

fn filter_display_folders(provider: Option<&ProviderType>, folders: Vec<Folder>) -> Vec<Folder> {
    if !matches!(provider, Some(ProviderType::Outlook)) {
        return folders;
    }
    folders
        .into_iter()
        .filter(|folder| !should_hide_stored_outlook_folder(folder))
        .collect()
}

/// 列出账户文件夹。与桌面端行为一致：首次 OAuth 后文件夹尚未同步时返回
/// 空列表（让侧栏保留占位文件夹）；缺本地 Archive 时补建。
pub async fn list_folders(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
    }
    let Args { account_id } = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_folders args: {e}")))?;
    let store = state.store.clone();
    let folders = run_blocking(move || {
        let provider = store
            .get_account(&account_id)?
            .map(|account| account.provider);
        let folders = store.list_folders(&account_id)?;

        if !provider_folders_have_arrived(&folders) {
            return Ok(Vec::new());
        }

        if should_seed_local_archive(&folders) {
            let archive = Folder {
                id: new_id(),
                account_id: account_id.clone(),
                remote_id: "__local_archive__".to_string(),
                name: "Archive".to_string(),
                folder_type: FolderType::Folder,
                role: Some(FolderRole::Archive),
                parent_id: None,
                color: None,
                is_system: true,
                sort_order: 3,
            };
            let _ = store.insert_folder(&archive);
            let folders = store.list_folders(&account_id)?;
            return Ok(filter_display_folders(provider.as_ref(), folders));
        }

        Ok(filter_display_folders(provider.as_ref(), folders))
    })
    .await?;
    serde_json::to_value(folders).map_err(ApiError::from_serialize)
}

pub async fn get_folder_unread_counts(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        account_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_folder_unread_counts args: {e}")))?;
    let store = state.store.clone();
    let counts = run_blocking(move || store.get_folder_unread_counts(&args.account_id)).await?;
    serde_json::to_value(counts).map_err(ApiError::from_serialize)
}

async fn discover_imap_folders(
    state: &AppStateRef,
    account_id: &str,
) -> Result<Vec<Folder>, ApiError> {
    let account = state
        .store
        .get_account(account_id)
        .map_err(ApiError::from_store)?
        .ok_or_else(|| ApiError::NotFound(format!("account not found: {account_id}")))?;
    if account.provider != ProviderType::Imap {
        return Err(ApiError::from_pebble(
            pebble_core::PebbleError::UnsupportedProvider(
                "Folder selection is currently available for IMAP accounts".to_string(),
            ),
        ));
    }
    let config = crate::sync::load_imap_config(&state.store, &state.crypto, account_id)?;
    let mut provider = ImapMailProvider::new(config);
    provider.set_account_id(account_id.to_string());
    provider.connect().await?;
    let result = provider.list_folders().await;
    if let Err(error) = provider.disconnect().await {
        tracing::debug!(
            "failed to disconnect IMAP folder discovery session for {account_id}: {error}"
        );
    }
    Ok(result?)
}

fn selected_remote_ids_for_settings(
    folders: &[Folder],
    configured: Option<Vec<String>>,
) -> Vec<String> {
    let configured = configured.map(|ids| ids.into_iter().collect::<HashSet<_>>());
    folders
        .iter()
        .filter(|folder| {
            folder.role == Some(FolderRole::Inbox)
                || folder.remote_id.eq_ignore_ascii_case("INBOX")
                || configured
                    .as_ref()
                    .is_none_or(|selected| selected.contains(&folder.remote_id))
        })
        .map(|folder| folder.remote_id.clone())
        .collect()
}

pub async fn get_imap_sync_folders(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        account_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_imap_sync_folders args: {e}")))?;
    let folders = discover_imap_folders(&state, &args.account_id).await?;
    let configured = state
        .store
        .get_sync_state(&args.account_id)
        .map_err(ApiError::from_store)?
        .and_then(|sync_state| sync_state.selected_imap_folder_remote_ids);
    let selected_remote_ids = selected_remote_ids_for_settings(&folders, configured);
    serde_json::to_value(ImapSyncFolderSettings {
        folders,
        selected_remote_ids,
    })
    .map_err(ApiError::from_serialize)
}

pub async fn update_imap_sync_folders(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        account_id: String,
        selected_remote_ids: Vec<String>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid update_imap_sync_folders args: {e}")))?;
    let folders = discover_imap_folders(&state, &args.account_id).await?;
    let available: HashSet<&str> = folders
        .iter()
        .map(|folder| folder.remote_id.as_str())
        .collect();
    let unknown: Vec<&str> = args
        .selected_remote_ids
        .iter()
        .map(String::as_str)
        .filter(|remote_id| !available.contains(remote_id))
        .collect();
    if !unknown.is_empty() {
        return Err(ApiError::from_pebble(pebble_core::PebbleError::Validation(
            format!("Unknown IMAP folders: {}", unknown.join(", ")),
        )));
    }
    let requested: HashSet<&str> = args
        .selected_remote_ids
        .iter()
        .map(String::as_str)
        .collect();
    let selected_remote_ids: Vec<String> = folders
        .iter()
        .filter(|folder| {
            folder.role == Some(FolderRole::Inbox)
                || folder.remote_id.eq_ignore_ascii_case("INBOX")
                || requested.contains(folder.remote_id.as_str())
        })
        .map(|folder| folder.remote_id.clone())
        .collect();
    if !folders.iter().any(|folder| {
        folder.role == Some(FolderRole::Inbox) || folder.remote_id.eq_ignore_ascii_case("INBOX")
    }) {
        return Err(ApiError::from_pebble(pebble_core::PebbleError::Validation(
            "The IMAP server did not return an Inbox folder".to_string(),
        )));
    }
    let store = state.store.clone();
    let account_id = args.account_id.clone();
    let remote_folders = folders.clone();
    let selected_for_store = selected_remote_ids.clone();
    let change = run_blocking(move || {
        store.apply_imap_folder_selection(&account_id, &remote_folders, &selected_for_store)
    })
    .await?;
    if let Err(error) =
        crate::command::lifecycle::refresh_search_documents(&state, &change.affected_message_ids)
    {
        tracing::warn!("failed to refresh search documents after changing IMAP folders: {error}");
    }
    let attachments_dir = state.attachments_dir.clone();
    if let Err(error) = tokio::task::spawn_blocking(move || {
        for message_id in change.deleted_message_ids {
            let message_dir = attachments_dir.join(&message_id);
            if message_dir.exists() {
                std::fs::remove_dir_all(&message_dir)
                    .map_err(|e| pebble_core::PebbleError::Internal(e.to_string()))?;
            }
        }
        Ok::<(), pebble_core::PebbleError>(())
    })
    .await
    .map_err(|e| ApiError::Internal(format!("attachment cleanup task failed: {e}")))?
    {
        tracing::warn!("failed to clean attachments after changing IMAP folders: {error}");
    }
    serde_json::to_value(ImapSyncFolderSettings {
        folders,
        selected_remote_ids,
    })
    .map_err(ApiError::from_serialize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(remote_id: &str, role: Option<FolderRole>) -> Folder {
        Folder {
            id: remote_id.to_string(),
            account_id: "account-1".to_string(),
            remote_id: remote_id.to_string(),
            name: remote_id.to_string(),
            folder_type: FolderType::Folder,
            role,
            parent_id: None,
            color: None,
            is_system: true,
            sort_order: 0,
        }
    }

    #[test]
    fn imap_selection_always_keeps_inbox() {
        let folders = vec![
            folder("INBOX", Some(FolderRole::Inbox)),
            folder("Archive", Some(FolderRole::Archive)),
            folder("Receipts", None),
        ];
        assert_eq!(
            selected_remote_ids_for_settings(&folders, Some(vec!["Receipts".to_string()])),
            vec!["INBOX", "Receipts"]
        );
    }

    #[test]
    fn missing_configuration_selects_all_discovered_folders() {
        let folders = vec![
            folder("INBOX", Some(FolderRole::Inbox)),
            folder("Archive", None),
        ];
        assert_eq!(
            selected_remote_ids_for_settings(&folders, None),
            vec!["INBOX", "Archive"]
        );
    }
}
