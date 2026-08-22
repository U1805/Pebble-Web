use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::ApiError;
use crate::state::AppStateRef;

/// 与桌面端 AppLogSnapshot 一致（source: src-tauri diagnostics.rs）。
#[derive(Debug, Serialize)]
pub struct AppLogSnapshot {
    pub path: String,
    pub content: String,
    pub truncated: bool,
}

/// 读取服务日志尾部（max_bytes 上限，对齐桌面端语义）。
/// 参数：{ max_bytes }（调用层已由前端 maxBytes 转换）。
pub async fn read_app_log(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            ApiError::BadRequest("invalid read_app_log args: expected max_bytes".into())
        })?
        .min(1024 * 1024); // 单次读取 1MB 上限，避免超大日志拖垮响应

    let snapshot =
        read_log_tail(&state.config.log_path(), max_bytes).map_err(ApiError::Internal)?;
    Ok(json!(snapshot))
}

fn read_log_tail(path: &Path, max_bytes: u64) -> Result<AppLogSnapshot, String> {
    let path_display = path.display().to_string();
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(AppLogSnapshot {
            path: path_display,
            content: String::new(),
            truncated: false,
        });
    };

    let file_len = metadata.len();
    let truncated = file_len > max_bytes;
    let start = if truncated { file_len - max_bytes } else { 0 };
    let mut file = fs::File::open(path).map_err(|e| format!("Failed to open app log: {e}"))?;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("Failed to seek app log: {e}"))?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read app log: {e}"))?;

    // 含 ANSI 色码的展示性日志，lossy 转换即可
    let content = String::from_utf8_lossy(&bytes).into_owned();

    Ok(AppLogSnapshot {
        path: path_display,
        content,
        truncated,
    })
}
