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
use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

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
    if base == "." || base == ".." {
        return "attachment".to_string();
    }

    let mut cleaned = base.to_string();
    while cleaned.contains("..") {
        cleaned = cleaned.replace("..", ".");
    }

    let sanitized: String = cleaned
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            _ => c,
        })
        .filter(|c| !c.is_control())
        .collect();
    let trimmed = sanitized
        .trim()
        .trim_matches(|c: char| c == '.' || c == ' ');
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

fn copy_attachment_file_safely(source: &Path, save_path: &Path) -> Result<PathBuf, PebbleError> {
    use std::io::{Read, Write};

    let mut src_file = std::fs::File::open(source)
        .map_err(|e| PebbleError::Internal(format!("Failed to open source: {e}")))?;
    let _total_bytes = src_file
        .metadata()
        .map_err(|e| PebbleError::Internal(format!("Failed to read file metadata: {e}")))?
        .len();

    let (actual_save_path, mut dst_file) = create_unique_target(save_path)?;
    let mut buf = [0u8; 8192];

    let copy_result: Result<(), PebbleError> = (|| {
        loop {
            let n = src_file
                .read(&mut buf)
                .map_err(|e| PebbleError::Internal(format!("Read error: {e}")))?;
            if n == 0 {
                break;
            }
            dst_file
                .write_all(&buf[..n])
                .map_err(|e| PebbleError::Internal(format!("Write error: {e}")))?;
        }
        dst_file
            .sync_all()
            .map_err(|e| PebbleError::Internal(format!("Failed to flush file: {e}")))?;
        Ok(())
    })();

    if let Err(error) = copy_result {
        drop(dst_file);
        let _ = std::fs::remove_file(&actual_save_path);
        return Err(error);
    }

    Ok(actual_save_path)
}

fn create_unique_target(save_path: &Path) -> Result<(PathBuf, std::fs::File), PebbleError> {
    const MAX_UNIQUE_ATTEMPTS: u32 = 1000;

    for attempt in 0..MAX_UNIQUE_ATTEMPTS {
        let candidate = unique_save_path(save_path, attempt);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(PebbleError::Internal(format!(
                    "Failed to create target file: {error}"
                )))
            }
        }
    }

    Err(PebbleError::Validation(
        "Could not choose an unused filename for attachment download".to_string(),
    ))
}

fn unique_save_path(save_path: &Path, attempt: u32) -> PathBuf {
    if attempt == 0 {
        return save_path.to_path_buf();
    }

    let parent = save_path.parent().unwrap_or_else(|| Path::new(""));
    let stem = save_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = save_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    parent.join(format!("{stem} ({attempt}){extension}"))
}

pub(crate) fn stage_compose_attachment_bytes(
    attachments_dir: &Path,
    filename: &str,
    bytes: &[u8],
) -> Result<PathBuf, PebbleError> {
    let staging_dir = attachments_dir.join("compose_staging");
    std::fs::create_dir_all(&staging_dir).map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to create compose attachment staging directory {}: {e}",
            staging_dir.display()
        ))
    })?;
    let canonical_staging_dir = staging_dir.canonicalize().map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to resolve compose attachment staging directory {}: {e}",
            staging_dir.display()
        ))
    })?;
    let safe_filename = sanitize_staged_filename(filename);
    let staged_dir = canonical_staging_dir.join(pebble_core::new_id());
    std::fs::create_dir_all(&staged_dir).map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to create compose attachment staging directory {}: {e}",
            staged_dir.display()
        ))
    })?;
    let staged_path = staged_dir.join(safe_filename);
    std::fs::write(&staged_path, bytes).map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to stage compose attachment {}: {e}",
            staged_path.display()
        ))
    })?;
    Ok(staged_path)
}

pub(crate) fn cleanup_staged_compose_attachment_path(
    attachments_dir: &Path,
    path: &Path,
) -> Result<(), PebbleError> {
    let staging_dir = attachments_dir.join("compose_staging");
    let canonical_staging_dir = staging_dir.canonicalize().map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to resolve compose attachment staging directory {}: {e}",
            staging_dir.display()
        ))
    })?;
    let canonical_path = path.canonicalize().map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to resolve staged compose attachment {}: {e}",
            path.display()
        ))
    })?;
    let parent = canonical_path.parent().ok_or_else(|| {
        PebbleError::Validation("Staged compose attachment has no parent directory".to_string())
    })?;
    if !canonical_path.starts_with(&canonical_staging_dir)
        || parent.parent() != Some(canonical_staging_dir.as_path())
        || !canonical_path.is_file()
    {
        return Err(PebbleError::Validation(format!(
            "Path is not a staged compose attachment: {}",
            canonical_path.display()
        )));
    }

    std::fs::remove_file(&canonical_path).map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to remove staged compose attachment {}: {e}",
            canonical_path.display()
        ))
    })?;
    if let Err(error) = std::fs::remove_dir(parent) {
        if error.kind() != std::io::ErrorKind::DirectoryNotEmpty
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(PebbleError::Internal(format!(
                "Failed to remove staged compose attachment directory {}: {error}",
                parent.display()
            )));
        }
    }
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
        PebbleError::Internal(format!(
            "Failed to create local attachment directory {}: {e}",
            message_dir.display()
        ))
    })?;
    let mut records = Vec::with_capacity(source_paths.len());
    for source in source_paths {
        let source_path = Path::new(source);
        let metadata = source_path.metadata().map_err(|e| {
            cleanup_local_attachment_records(&records);
            PebbleError::Internal(format!(
                "Attachment source file not available: {source} ({e})"
            ))
        })?;
        if !metadata.is_file() {
            cleanup_local_attachment_records(&records);
            return Err(PebbleError::Validation(format!(
                "Attachment source is not a file: {source}"
            )));
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
                "Failed to create local attachment directory {}: {error}",
                attachment_dir.display()
            )));
        }
        let target = attachment_dir.join(&filename);
        let staged_path = match copy_attachment_file_safely(source_path, &target) {
            Ok(path) => path,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&attachment_dir);
                cleanup_local_attachment_records(&records);
                return Err(error);
            }
        };
        let size = std::fs::metadata(&staged_path)
            .map(|metadata| metadata.len().min(i64::MAX as u64) as i64)
            .unwrap_or(0);
        records.push(Attachment {
            id: pebble_core::new_id(),
            message_id: message_id.to_string(),
            filename,
            mime_type: "application/octet-stream".to_string(),
            size,
            local_path: Some(staged_path.to_string_lossy().into_owned()),
            content_id: None,
            is_inline: false,
        });
    }
    Ok(records)
}

pub(crate) fn cleanup_local_attachment_records(records: &[Attachment]) {
    for record in records {
        let Some(path) = record.local_path.as_deref().map(Path::new) else {
            continue;
        };
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %path.display(),
                    "Failed to remove staged attachment: {error}"
                );
            }
        }
        if let Some(parent) = path.parent() {
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
        .ok_or_else(|| PebbleError::Internal("Attachment not found".to_string()))
        .map_err(ApiError::from_pebble)?;

    let attachments_dir = state.attachments_dir.clone();
    let stored_path = attachment
        .local_path
        .as_deref()
        .ok_or_else(|| PebbleError::Internal("Attachment file not available".to_string()))
        .map_err(ApiError::from_pebble)?;
    let file_path = if Path::new(stored_path).is_absolute() {
        PathBuf::from(stored_path)
    } else {
        // Web 服务只允许从自己的附件根目录读取文件。
        attachments_dir.join(stored_path)
    };

    // Web HTTP 端点的额外安全边界：源文件必须位于 Pebble 附件目录。
    let allowed_dir = attachments_dir
        .canonicalize()
        .unwrap_or_else(|_| attachments_dir.clone());
    tracing::debug!(%attachment_id, file_path = %file_path.display(), allowed = %allowed_dir.display(), "download attachment resolve");
    let canonical_path = file_path.canonicalize().map_err(|e| {
        ApiError::from_pebble(PebbleError::Internal(format!("Failed to open source: {e}")))
    })?;
    if !canonical_path.starts_with(&allowed_dir) {
        return Err(ApiError::BadRequest(
            "attachment path outside allowed directory".to_string(),
        ));
    }

    let file = tokio::fs::File::open(&canonical_path)
        .await
        .map_err(|e| {
            ApiError::from_pebble(PebbleError::Internal(format!("Failed to open source: {e}")))
        })?;
    let actual_size = file
        .metadata()
        .await
        .map_err(|e| {
            ApiError::from_pebble(PebbleError::Internal(format!(
                "Failed to read file metadata: {e}"
            )))
        })?
        .len();
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mime = attachment.mime_type.clone();
    let disposition = content_disposition_attachment(&attachment.filename);

    Ok(Response::builder()
        .header(
            header::CONTENT_TYPE,
            if mime.is_empty() {
                "application/octet-stream"
            } else {
                &mime
            },
        )
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CONTENT_LENGTH, actual_size.to_string())
        .body(body)
        .map_err(|e| ApiError::Internal(format!("response build failed: {e}")))?)
}

fn content_disposition_attachment(name: &str) -> String {
    let safe_name = name.replace(['"', '\r', '\n'], "_");
    let ascii_fallback = safe_name
        .chars()
        .map(|ch| if ch.is_ascii() && !ch.is_ascii_control() { ch } else { '_' })
        .collect::<String>();
    let encoded = percent_encode_header_value(safe_name.as_bytes());
    format!(
        "attachment; filename=\"{ascii_fallback}\"; filename*=UTF-8''{encoded}"
    )
}

fn percent_encode_header_value(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len());
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}
