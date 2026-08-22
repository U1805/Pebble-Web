use std::time::Duration;

use serde_json::{json, Value};

use crate::error::ApiError;
use crate::state::AppStateRef;

/// 更新检查源：个人 Pebble Web fork 的 GitHub Release。
///（Web 端"更新"= 有新版本 pebble-web fork 发布；桌面端指向上游 Pebble release）
const UPDATE_REPO: &str = "U1805/Pebble-Web";

/// 查询 GitHub Releases 最新版本，与当前运行版本比较。
/// 返回结构对齐桌面端 AboutTab 的 check_for_update 期望。
pub async fn check_for_update(_state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let current = env!("CARGO_PKG_VERSION");
    let current_v =
        semver::Version::parse(current).unwrap_or_else(|_| semver::Version::new(0, 0, 0));

    let url = format!("https://api.github.com/repos/{UPDATE_REPO}/releases/latest");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| ApiError::Internal(format!("failed to build http client: {e}")))?;

    let resp = client
        .get(&url)
        .header("User-Agent", "pebble-web")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| ApiError::Internal(format!("update check request failed: {e}")))?;

    if !resp.status().is_success() {
        // GitHub 对无 release 的仓库返回 404：视为「无可用更新」，不向用户报错
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(
                json!({ "latest_version": "", "release_url": format!("https://github.com/{UPDATE_REPO}/releases"), "is_newer": false }),
            );
        }
        return Err(ApiError::Internal(format!(
            "update check failed: HTTP {}",
            resp.status()
        )));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ApiError::Internal(format!("update check response invalid: {e}")))?;

    let latest = body["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    let release_url = body["html_url"]
        .as_str()
        .unwrap_or(&format!("https://github.com/{UPDATE_REPO}/releases"))
        .to_string();

    let is_newer = semver::Version::parse(&latest)
        .map(|v| v > current_v)
        .unwrap_or(false);

    Ok(json!({
        "latest_version": latest,
        "release_url": release_url,
        "is_newer": is_newer,
    }))
}
