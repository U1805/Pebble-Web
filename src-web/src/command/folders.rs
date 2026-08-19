use pebble_core::{new_id, Folder, FolderRole, FolderType, ProviderType};
use pebble_mail::should_hide_outlook_folder;
use serde_json::Value;

use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

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
    let account_id: String = serde_json::from_value(args)
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