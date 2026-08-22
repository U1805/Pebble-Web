use axum::{
    body::Body,
    extract::{Multipart, Path as AxumPath, State},
    http::{header, HeaderMap},
    response::Response,
    Json,
};
use pebble_core::{Attachment, PebbleError};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;
use tokio_util::io::ReaderStream;

use crate::auth;
use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

const MAX_STAGED_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const MAX_STAGED_ATTACHMENT_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
pub(crate) const MAX_MULTIPART_BODY_BYTES: usize = 100 * 1024 * 1024;
static COMPOSE_STAGING_LOCK: Mutex<()> = Mutex::const_new(());

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

fn is_windows_reserved_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Keep staged names inert across platforms; the path is always generated below
/// the server-owned compose_staging directory.
fn sanitize_staged_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut cleaned = base.replace("..", ".");
    cleaned = cleaned
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            _ => c,
        })
        .filter(|c| !c.is_control())
        .collect();
    let trimmed = cleaned.trim().trim_matches(|c: char| c == '.' || c == ' ');
    if trimmed.is_empty() {
        return "attachment".to_string();
    }
    let stem = Path::new(trimmed)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if is_windows_reserved_name(stem) {
        return "attachment".to_string();
    }
    trimmed.to_string()
}

fn staged_total_bytes(staging_dir: &Path) -> Result<u64, PebbleError> {
    let mut total = 0_u64;
    let entries = match std::fs::read_dir(staging_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(PebbleError::Internal(format!(
                "Failed to inspect compose staging directory: {error}"
            )))
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| {
            PebbleError::Internal(format!("Failed to inspect staged attachment: {e}"))
        })?;
        let metadata = entry.metadata().map_err(|e| {
            PebbleError::Internal(format!("Failed to inspect staged attachment: {e}"))
        })?;
        if metadata.is_dir() {
            for child in std::fs::read_dir(entry.path()).map_err(|e| {
                PebbleError::Internal(format!("Failed to inspect staged attachment: {e}"))
            })? {
                let child = child.map_err(|e| {
                    PebbleError::Internal(format!("Failed to inspect staged attachment: {e}"))
                })?;
                let child_metadata = child.metadata().map_err(|e| {
                    PebbleError::Internal(format!("Failed to inspect staged attachment: {e}"))
                })?;
                if child_metadata.is_file() {
                    total = total.saturating_add(child_metadata.len());
                }
            }
        }
    }
    Ok(total)
}

pub(crate) fn stage_compose_attachment_bytes(
    attachments_dir: &Path,
    filename: &str,
    bytes: &[u8],
) -> Result<PathBuf, PebbleError> {
    if bytes.len() > MAX_STAGED_ATTACHMENT_BYTES {
        return Err(PebbleError::Validation(format!(
            "Attachment exceeds the {} MiB per-file limit",
            MAX_STAGED_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }
    let staging_dir = attachments_dir.join("compose_staging");
    std::fs::create_dir_all(&staging_dir).map_err(|e| {
        PebbleError::Internal(format!("Failed to create compose staging directory: {e}"))
    })?;
    let staging_dir = staging_dir.canonicalize().map_err(|e| {
        PebbleError::Internal(format!("Failed to resolve compose staging directory: {e}"))
    })?;
    let total = staged_total_bytes(&staging_dir)?;
    if total.saturating_add(bytes.len() as u64) > MAX_STAGED_ATTACHMENT_TOTAL_BYTES {
        return Err(PebbleError::Validation(format!(
            "Compose staging exceeds the {} MiB total limit",
            MAX_STAGED_ATTACHMENT_TOTAL_BYTES / (1024 * 1024)
        )));
    }
    let staged_dir = staging_dir.join(pebble_core::new_id());
    std::fs::create_dir_all(&staged_dir).map_err(|e| {
        PebbleError::Internal(format!("Failed to create staged attachment directory: {e}"))
    })?;
    let staged_path = staged_dir.join(sanitize_staged_filename(filename));
    if let Err(error) = std::fs::write(&staged_path, bytes) {
        let _ = std::fs::remove_dir_all(&staged_dir);
        return Err(PebbleError::Internal(format!(
            "Failed to stage compose attachment: {error}"
        )));
    }
    Ok(staged_path)
}

pub(crate) fn cleanup_staged_compose_attachment_path(
    attachments_dir: &Path,
    path: &Path,
) -> Result<(), PebbleError> {
    let staging_dir = attachments_dir.join("compose_staging");
    let canonical_staging_dir = staging_dir.canonicalize().map_err(|e| {
        PebbleError::Internal(format!("Failed to resolve compose staging directory: {e}"))
    })?;
    let canonical_path = path
        .canonicalize()
        .map_err(|e| PebbleError::Internal(format!("Failed to resolve staged attachment: {e}")))?;
    let parent = canonical_path.parent().ok_or_else(|| {
        PebbleError::Validation("Staged attachment has no parent directory".to_string())
    })?;
    if !canonical_path.starts_with(&canonical_staging_dir)
        || parent.parent() != Some(canonical_staging_dir.as_path())
        || !canonical_path.is_file()
    {
        return Err(PebbleError::Validation(
            "Path is not a staged compose attachment".to_string(),
        ));
    }
    std::fs::remove_file(&canonical_path)
        .map_err(|e| PebbleError::Internal(format!("Failed to remove staged attachment: {e}")))?;
    let _ = std::fs::remove_dir(parent);
    Ok(())
}

pub(crate) fn validate_staged_attachment_paths(
    attachments_dir: &Path,
    paths: &[String],
) -> Result<Vec<String>, PebbleError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let staging_dir = attachments_dir
        .join("compose_staging")
        .canonicalize()
        .map_err(|e| {
            PebbleError::Internal(format!("Failed to resolve compose staging directory: {e}"))
        })?;
    let mut validated = Vec::with_capacity(paths.len());
    for raw_path in paths {
        let canonical = Path::new(raw_path).canonicalize().map_err(|e| {
            PebbleError::Internal(format!("Attachment path not found: {raw_path} ({e})"))
        })?;
        let parent = canonical.parent();
        if !canonical.starts_with(&staging_dir)
            || parent.and_then(Path::parent) != Some(staging_dir.as_path())
            || !canonical.is_file()
        {
            return Err(PebbleError::Validation(
                "Attachment path is outside compose staging".to_string(),
            ));
        }
        validated.push(canonical.to_string_lossy().into_owned());
    }
    Ok(validated)
}

pub(crate) fn stage_local_attachment_records(
    attachments_root: &Path,
    message_id: &str,
    source_paths: &[String],
) -> Result<Vec<Attachment>, PebbleError> {
    if source_paths.is_empty() {
        return Ok(Vec::new());
    }
    let message_dir = attachments_root.join(message_id);
    std::fs::create_dir_all(&message_dir).map_err(|e| {
        PebbleError::Internal(format!("Failed to create local attachment directory: {e}"))
    })?;
    let mut records = Vec::with_capacity(source_paths.len());
    for source in source_paths {
        let source_path = Path::new(source);
        let metadata = source_path.metadata().map_err(|e| {
            cleanup_local_attachment_records(&records);
            PebbleError::Internal(format!("Attachment source not available: {source} ({e})"))
        })?;
        if !metadata.is_file() {
            cleanup_local_attachment_records(&records);
            return Err(PebbleError::Validation(
                "Attachment source is not a file".to_string(),
            ));
        }
        let filename = sanitize_staged_filename(
            source_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("attachment"),
        );
        let attachment_dir = message_dir.join(pebble_core::new_id());
        if let Err(error) = std::fs::create_dir_all(&attachment_dir) {
            cleanup_local_attachment_records(&records);
            return Err(PebbleError::Internal(format!(
                "Failed to create local attachment directory: {error}"
            )));
        }
        let target = attachment_dir.join(&filename);
        if let Err(error) = std::fs::copy(source_path, &target) {
            let _ = std::fs::remove_dir_all(&attachment_dir);
            cleanup_local_attachment_records(&records);
            return Err(PebbleError::Internal(format!(
                "Failed to copy attachment: {error}"
            )));
        }
        records.push(Attachment {
            id: pebble_core::new_id(),
            message_id: message_id.to_string(),
            filename,
            mime_type: "application/octet-stream".to_string(),
            size: metadata.len().min(i64::MAX as u64) as i64,
            local_path: Some(target.to_string_lossy().into_owned()),
            content_id: None,
            is_inline: false,
        });
    }
    Ok(records)
}

pub(crate) fn cleanup_local_attachment_records(records: &[Attachment]) {
    for record in records {
        let Some(path) = record.local_path.as_deref() else {
            continue;
        };
        let _ = std::fs::remove_file(path);
        if let Some(parent) = Path::new(path).parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

/// Browser upload endpoint for compose attachments.
///
/// The command-style JSON endpoint remains available for desktop parity, but
/// browsers must not expand binary data into a JSON number array: that makes
/// ordinary 25 MiB attachments exceed the server's buffered request limit.
pub async fn stage_compose_attachment_multipart(
    State(state): State<AppStateRef>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiError> {
    auth::require_auth(&state, &headers)?;

    let mut filename = None;
    let mut file_bytes = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::BadRequest(format!("invalid multipart upload: {error}")))?
    {
        let field_name = field.name().map(str::to_owned);
        let field_filename = field.file_name().map(str::to_owned);
        match field_name.as_deref() {
            Some("filename") => {
                filename = Some(field.text().await.map_err(|error| {
                    ApiError::BadRequest(format!("invalid attachment filename: {error}"))
                })?);
            }
            Some("file") => {
                if filename.is_none() {
                    filename = field_filename;
                }
                file_bytes = Some(field.bytes().await.map_err(|error| {
                    ApiError::BadRequest(format!("invalid attachment bytes: {error}"))
                })?);
            }
            _ => {}
        }
    }

    let filename = filename
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "attachment".to_string());
    let bytes = file_bytes.ok_or_else(|| {
        ApiError::BadRequest("multipart upload is missing the file field".to_string())
    })?;

    let _guard = COMPOSE_STAGING_LOCK.lock().await;
    let attachments_dir = state.attachments_dir.clone();
    let path =
        run_blocking(move || stage_compose_attachment_bytes(&attachments_dir, &filename, &bytes))
            .await?;
    Ok(Json(Value::String(path.to_string_lossy().into_owned())))
}

pub async fn stage_compose_attachment(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        filename: String,
        bytes: Vec<u8>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid stage_compose_attachment args: {e}")))?;
    let _guard = COMPOSE_STAGING_LOCK.lock().await;
    let attachments_dir = state.attachments_dir.clone();
    let path = run_blocking(move || {
        stage_compose_attachment_bytes(&attachments_dir, &args.filename, &args.bytes)
    })
    .await?;
    serde_json::to_value(path.to_string_lossy().into_owned()).map_err(ApiError::from_serialize)
}

pub async fn cleanup_staged_compose_attachment(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        path: String,
    }
    let args: Args = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!(
            "invalid cleanup_staged_compose_attachment args: {e}"
        ))
    })?;
    let _guard = COMPOSE_STAGING_LOCK.lock().await;
    let attachments_dir = state.attachments_dir.clone();
    run_blocking(move || {
        cleanup_staged_compose_attachment_path(&attachments_dir, Path::new(&args.path))
    })
    .await?;
    Ok(Value::Null)
}

pub async fn get_attachment_path(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        attachment_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_attachment_path args: {e}")))?;
    let store = state.store.clone();
    let path = run_blocking(move || {
        Ok(store
            .get_attachment(&args.attachment_id)?
            .and_then(|a| a.local_path))
    })
    .await?;
    serde_json::to_value(path).map_err(ApiError::from_serialize)
}

/// 附件下载端点：GET /api/v1/attachments/{attachment_id}/download（Bearer 鉴权）。
///
/// 浏览器下载走 fetch→blob（token 只能放 header，不放 URL query）。
/// 响应带 Content-Disposition attachment，文件名经 sanitize 防止路径注入。
pub async fn download_attachment(
    State(state): State<AppStateRef>,
    AxumPath(attachment_id): AxumPath<String>,
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
        .header(
            header::CONTENT_TYPE,
            if mime.is_empty() {
                "application/octet-stream"
            } else {
                &mime
            },
        )
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CONTENT_LENGTH, attachment.size.to_string())
        .body(body)
        .map_err(|e| ApiError::Internal(format!("response build failed: {e}")))?)
}

/// Content-Disposition 文件名清洗：去掉引号与 CR/LF，防 header 注入。
fn sanitize_filename_header(name: &str) -> String {
    name.replace(['"', '\r', '\n'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pw-{label}-{}", pebble_core::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn staged_filename_is_platform_safe_and_non_empty() {
        assert_eq!(sanitize_staged_filename("../report?.txt"), "report_.txt");
        assert_eq!(sanitize_staged_filename("C:\\CON.txt"), "attachment");
        assert_eq!(sanitize_staged_filename("...   "), "attachment");
        assert_eq!(
            sanitize_staged_filename("nested\\folder/file.txt"),
            "file.txt"
        );
    }

    #[test]
    fn staged_attachment_enforces_per_file_limit() {
        let root = temp_dir("attachment-limit");
        let bytes = vec![0_u8; MAX_STAGED_ATTACHMENT_BYTES + 1];
        let error = stage_compose_attachment_bytes(&root, "large.bin", &bytes).unwrap_err();
        assert!(error.to_string().contains("per-file limit"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_attachment_records_copy_and_cleanup_files() {
        let root = temp_dir("attachment-copy");
        let source = root.join("source.txt");
        std::fs::write(&source, b"payload").unwrap();
        let records = stage_local_attachment_records(
            &root.join("attachments"),
            "message-1",
            &[source.to_string_lossy().into_owned()],
        )
        .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message_id, "message-1");
        assert_eq!(records[0].size, 7);
        let copied = PathBuf::from(records[0].local_path.as_ref().unwrap());
        assert_eq!(std::fs::read(&copied).unwrap(), b"payload");

        cleanup_local_attachment_records(&records);
        assert!(!copied.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn staged_path_validation_rejects_paths_outside_staging() {
        let root = temp_dir("attachment-validation");
        let staging = root.join("compose_staging").join("stage-1");
        std::fs::create_dir_all(&staging).unwrap();
        let valid = staging.join("ok.txt");
        std::fs::write(&valid, b"ok").unwrap();
        let outside = root.join("outside.txt");
        std::fs::write(&outside, b"no").unwrap();

        let accepted =
            validate_staged_attachment_paths(&root, &[valid.to_string_lossy().into_owned()])
                .unwrap();
        assert_eq!(accepted.len(), 1);
        let error =
            validate_staged_attachment_paths(&root, &[outside.to_string_lossy().into_owned()])
                .unwrap_err();
        assert!(error.to_string().contains("outside compose staging"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn attachment_header_sanitizes_injection_characters() {
        assert_eq!(sanitize_filename_header("a\"\r\nb.txt"), "a___b.txt");
    }
}
