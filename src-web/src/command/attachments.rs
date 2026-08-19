use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap},
    response::Response,
};
use serde_json::Value;
use tokio_util::io::ReaderStream;

use crate::auth;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 列出邮件的附件。
#[derive(serde::Deserialize)]
pub struct ListAttachmentsRequest {
    pub message_id: String,
}

pub async fn list_attachments(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let req: ListAttachmentsRequest = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_attachments args: {e}")))?;
    let attachments = state
        .store
        .list_attachments_by_message(&req.message_id)
        .map_err(ApiError::from_store)?;
    serde_json::to_value(attachments).map_err(ApiError::from_serialize)
}

/// 附件下载端点：GET /api/v1/attachments/{attachment_id}/download（Bearer 鉴权）。
///
/// 浏览器下载走 fetch→blob（token 只能放 header，不放 URL query）。
/// 响应带 Content-Disposition attachment，文件名经 sanitize 防止路径注入。
pub async fn download_attachment(
    State(state): State<AppStateRef>,
    Path(attachment_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    auth::require_auth(&state, &headers)?;

    let attachment = state
        .store
        .get_attachment(&attachment_id)
        .map_err(ApiError::from_store)?
        .ok_or_else(|| ApiError::NotFound("attachment not found".to_string()))?;

    let attachments_dir = state.attachments_dir.clone();
    let file_path = match &attachment.local_path {
        Some(path) if std::path::Path::new(path).is_absolute() => std::path::PathBuf::from(path),
        // 相对路径（历史数据或测试数据）按附件根目录解析
        Some(path) => attachments_dir.join(path),
        None => {
            // 无 local_path（如未落盘）时按约定路径推导：attachments/{message_id}/{filename}
            let safe_filename = std::path::Path::new(&attachment.filename)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("attachment")
                .to_string();
            attachments_dir
                .join(&attachment.message_id)
                .join(safe_filename)
        }
    };

    // 防路径穿越：解析后必须位于附件根目录内
    let allowed_dir = attachments_dir
        .canonicalize()
        .unwrap_or_else(|_| attachments_dir.clone());
    tracing::debug!(%attachment_id, file_path = %file_path.display(), allowed = %allowed_dir.display(), "download attachment resolve");
    let canonical_path = file_path
        .canonicalize()
        .map_err(|_| ApiError::NotFound("attachment file not found".to_string()))?;
    if !canonical_path.starts_with(&allowed_dir) {
        return Err(ApiError::BadRequest(
            "attachment path outside allowed directory".to_string(),
        ));
    }

    let file = tokio::fs::File::open(&canonical_path)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to open file: {e}")))?;
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mime = attachment.mime_type.clone();
    let filename = sanitize_filename_header(&attachment.filename);

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, if mime.is_empty() { "application/octet-stream" } else { &mime })
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\""))
        .header(header::CONTENT_LENGTH, attachment.size.to_string())
        .body(body)
        .map_err(|e| ApiError::Internal(format!("response build failed: {e}")))?)
}

/// Content-Disposition 文件名清洗：去掉引号与 CR/LF，防 header 注入。
fn sanitize_filename_header(name: &str) -> String {
    name.replace(['"', '\r', '\n'], "_")
}