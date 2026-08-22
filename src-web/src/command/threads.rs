use serde_json::Value;

use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 列出文件夹内的线程摘要。folder_ids 非空时按多文件夹查询。
pub async fn list_threads(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        folder_id: String,
        #[serde(default)]
        folder_ids: Option<Vec<String>>,
        limit: u32,
        offset: u32,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_threads args: {e}")))?;
    let store = state.store.clone();
    let threads = run_blocking(move || match args.folder_ids {
        Some(ids) if !ids.is_empty() => {
            store.list_threads_by_folders(&ids, args.limit, args.offset)
        }
        _ => store.list_threads_by_folder(&args.folder_id, args.limit, args.offset),
    })
    .await?;
    serde_json::to_value(threads).map_err(ApiError::from_serialize)
}

/// 列出线程内全部消息（按时间序）。
pub async fn list_thread_messages(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let thread_id: String = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_thread_messages args: {e}")))?;
    let store = state.store.clone();
    let messages = run_blocking(move || store.list_messages_by_thread(&thread_id)).await?;
    serde_json::to_value(messages).map_err(ApiError::from_serialize)
}
