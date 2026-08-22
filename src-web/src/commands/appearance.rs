use axum::{
    body::Body,
    extract::{Path as AxumPath, State},
    http::header,
    response::Response,
};
use pebble_core::{new_id, PebbleError};
use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio_util::io::ReaderStream;

use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

const MAX_BACKGROUND_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const BACKGROUND_IMAGE_URL_PREFIX: &str = "/api/v1/background-images/";

#[derive(Debug, Clone, Serialize)]
pub struct ImportedBackgroundImage {
    pub path: String,
    pub filename: String,
    pub size: u64,
}

fn detect_extension(bytes: &[u8]) -> Result<&'static str, PebbleError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Ok("jpg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok("gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok("webp");
    }
    Err(PebbleError::Validation(
        "Background image must be PNG, JPEG, GIF, or WebP".to_string(),
    ))
}

fn validate_bytes(bytes: &[u8]) -> Result<&'static str, PebbleError> {
    if bytes.is_empty() {
        return Err(PebbleError::Validation(
            "Background image file is empty".to_string(),
        ));
    }
    if bytes.len() > MAX_BACKGROUND_IMAGE_BYTES {
        return Err(PebbleError::Validation(format!(
            "Background image is too large (max {} MB)",
            MAX_BACKGROUND_IMAGE_BYTES / 1024 / 1024
        )));
    }
    detect_extension(bytes)
}

fn validate_stored_filename(filename: &str) -> Result<String, PebbleError> {
    let path = Path::new(filename);
    let basename = path.file_name().and_then(|value| value.to_str());
    if basename != Some(filename) || !filename.starts_with("background-") {
        return Err(PebbleError::Validation(
            "Invalid background image filename".to_string(),
        ));
    }
    let extension = path.extension().and_then(|value| value.to_str());
    if !matches!(extension, Some("png" | "jpg" | "gif" | "webp")) {
        return Err(PebbleError::Validation(
            "Invalid background image filename extension".to_string(),
        ));
    }
    Ok(filename.to_string())
}

fn filename_from_client_path(path: &str) -> Result<String, PebbleError> {
    let filename = path
        .strip_prefix(BACKGROUND_IMAGE_URL_PREFIX)
        .ok_or_else(|| PebbleError::Validation("Invalid background image path".to_string()))?;
    validate_stored_filename(filename)
}

fn backgrounds_dir(state: &AppStateRef) -> PathBuf {
    state.config.data_dir.join("backgrounds")
}

fn write_background_image(
    backgrounds_dir: &Path,
    bytes: &[u8],
) -> Result<ImportedBackgroundImage, PebbleError> {
    let extension = validate_bytes(bytes)?;
    std::fs::create_dir_all(backgrounds_dir).map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to create background image directory {}: {e}",
            backgrounds_dir.display()
        ))
    })?;

    let filename = format!("background-{}.{}", new_id(), extension);
    let path = backgrounds_dir.join(&filename);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| PebbleError::Internal(format!("Failed to create background image: {e}")))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&path);
        return Err(PebbleError::Internal(format!(
            "Failed to write background image: {error}"
        )));
    }

    Ok(ImportedBackgroundImage {
        path: format!("{BACKGROUND_IMAGE_URL_PREFIX}{filename}"),
        filename,
        size: bytes.len() as u64,
    })
}

fn delete_background_image_file(backgrounds_dir: &Path, filename: &str) -> Result<(), PebbleError> {
    let filename = validate_stored_filename(filename)?;
    std::fs::create_dir_all(backgrounds_dir).map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to create background image directory {}: {e}",
            backgrounds_dir.display()
        ))
    })?;
    let canonical_dir = backgrounds_dir.canonicalize().map_err(|e| {
        PebbleError::Internal(format!(
            "Failed to resolve background image directory {}: {e}",
            backgrounds_dir.display()
        ))
    })?;
    let target = backgrounds_dir.join(filename);
    let canonical_target = match target.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(PebbleError::Internal(format!(
                "Failed to resolve background image path: {error}"
            )))
        }
    };
    if !canonical_target.starts_with(&canonical_dir) {
        return Err(PebbleError::Validation(
            "Background image path is outside Pebble's background directory".to_string(),
        ));
    }
    if canonical_target.is_file() {
        std::fs::remove_file(canonical_target).map_err(|e| {
            PebbleError::Internal(format!("Failed to delete background image: {e}"))
        })?;
    }
    Ok(())
}

pub async fn import_background_image(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        #[allow(dead_code)]
        filename: String,
        bytes: Vec<u8>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid import_background_image args: {e}")))?;
    let dir = backgrounds_dir(&state);
    let imported = run_blocking(move || write_background_image(&dir, &args.bytes)).await?;
    serde_json::to_value(imported).map_err(ApiError::from_serialize)
}

pub async fn delete_background_image(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        path: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid delete_background_image args: {e}")))?;
    let filename = filename_from_client_path(&args.path).map_err(ApiError::from_pebble)?;
    let dir = backgrounds_dir(&state);
    run_blocking(move || delete_background_image_file(&dir, &filename)).await?;
    Ok(Value::Null)
}

/// Browser CSS cannot attach a Bearer header. The mutation commands remain
/// authenticated, while this endpoint serves only opaque randomly named files
/// from the background-image directory.
pub async fn get_background_image(
    State(state): State<AppStateRef>,
    AxumPath(filename): AxumPath<String>,
) -> Result<Response, ApiError> {
    let filename = validate_stored_filename(&filename).map_err(ApiError::from_pebble)?;
    let dir = backgrounds_dir(&state);
    let allowed_dir = dir
        .canonicalize()
        .map_err(|_| ApiError::NotFound("background image not found".to_string()))?;
    let path = dir.join(filename);
    let canonical_path = path
        .canonicalize()
        .map_err(|_| ApiError::NotFound("background image not found".to_string()))?;
    if !canonical_path.starts_with(&allowed_dir) {
        return Err(ApiError::BadRequest(
            "background image path outside allowed directory".to_string(),
        ));
    }
    let file = tokio::fs::File::open(&canonical_path)
        .await
        .map_err(|_| ApiError::NotFound("background image not found".to_string()))?;
    let mime = match canonical_path.extension().and_then(|value| value.to_str()) {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    };
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|e| ApiError::Internal(format!("response build failed: {e}")))
}
