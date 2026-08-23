use serde::Serialize;
use serde_json::Value;

use crate::error::ApiError;
use crate::state::AppStateRef;

#[derive(Serialize)]
pub struct UpdateInfo {
    pub latest_version: String,
    pub release_url: String,
    pub is_newer: bool,
}

/// Web uses server-side HTTP instead of the desktop process, but the update
/// source and comparison semantics intentionally match the Tauri command.
pub async fn check_for_update(_state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        current_version: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid check_for_update args: {e}")))?;

    let client = reqwest::Client::builder()
        .user_agent("Pebble-Email-Client")
        .build()
        .map_err(|e| ApiError::Internal(format!("Failed to create HTTP client: {e}")))?;

    let response = client
        .get("https://api.github.com/repos/QingJ01/Pebble/releases/latest")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to check for updates: {e}")))?;

    if !response.status().is_success() {
        return Err(ApiError::Internal(format!(
            "GitHub API returned status {}",
            response.status()
        )));
    }

    let data: serde_json::Value = response
        .json()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to parse response: {e}")))?;

    let tag = data["tag_name"]
        .as_str()
        .ok_or_else(|| ApiError::Internal("Missing tag_name in response".to_string()))?;
    let latest = tag.trim_start_matches('v').to_string();
    let release_url = data["html_url"]
        .as_str()
        .unwrap_or("https://github.com/QingJ01/Pebble/releases")
        .to_string();

    let is_newer = match (
        semver::Version::parse(&latest),
        semver::Version::parse(&args.current_version),
    ) {
        (Ok(latest_version), Ok(current_version)) => latest_version > current_version,
        _ => latest != args.current_version,
    };

    serde_json::to_value(UpdateInfo {
        latest_version: latest,
        release_url,
        is_newer,
    })
    .map_err(ApiError::from_serialize)
}

/// Shared command health check. The HTTP health endpoint remains available for
/// container/runtime health probes.
pub async fn health_check_command(state: AppStateRef) -> Result<Value, ApiError> {
    let accounts = state
        .store
        .list_accounts()
        .map_err(|e| ApiError::Internal(format!("Health check failed: {e}")))?;
    Ok(Value::String(format!(
        "Pebble is healthy. {} account(s) configured.",
        accounts.len()
    )))
}
