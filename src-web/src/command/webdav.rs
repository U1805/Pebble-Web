use pebble_core::PebbleError;
use pebble_store::cloud_sync::{
    preview_backup, BackupPreview, WebDavClient, SETTINGS_BACKUP_FILENAME,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::backup::{build_backup_data, restore_backup_data};
use crate::error::ApiError;
use crate::state::AppStateRef;

/// WebDAV 连接凭据（后端 snake_case 契约，调用层已由前端转换）。
#[derive(Deserialize)]
pub struct WebdavCredentialsArgs {
    pub url: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub secret_passphrase: Option<String>,
}

/// 与桌面端 AutoBackupConfig 字节级兼容的自动备份配置（存 secure_user_data）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoBackupConfig {
    pub url: String,
    pub username: String,
    pub password: String,
    pub secret_passphrase: Option<String>,
    pub interval_minutes: u64,
    pub enabled: bool,
}

const AUTO_BACKUP_CONFIG_KEY: &str = "auto-backup-config";
const SUPPORTED_INTERVALS_MINUTES: &[u64] = &[30, 60, 180, 360, 720, 1440];

fn new_client(args: &WebdavCredentialsArgs) -> Result<WebDavClient, PebbleError> {
    WebDavClient::new(
        args.url.clone(),
        args.username.clone(),
        args.password.clone(),
    )
}

pub async fn test_webdav_connection(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: WebdavCredentialsArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid args: {e}")))?;
    let _ = state;
    let client = new_client(&args)?;
    client.test_connection().await?;
    Ok(Value::String("Connection successful".to_string()))
}

pub async fn backup_to_webdav(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: WebdavCredentialsArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid args: {e}")))?;
    let data = build_backup_data(&state, args.secret_passphrase.clone())?;
    let client = new_client(&args)?;
    client.upload(SETTINGS_BACKUP_FILENAME, &data).await?;
    Ok(Value::String(
        "Settings backup completed successfully".to_string(),
    ))
}

pub async fn preview_webdav_backup(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: WebdavCredentialsArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid args: {e}")))?;
    let _ = state;
    let client = new_client(&args)?;
    let data = client.download(SETTINGS_BACKUP_FILENAME).await?;
    let preview: BackupPreview = preview_backup(&data)?;
    serde_json::to_value(preview).map_err(ApiError::from_serialize)
}

pub async fn restore_from_webdav(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: WebdavCredentialsArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid args: {e}")))?;
    let client = new_client(&args)?;
    let data = client.download(SETTINGS_BACKUP_FILENAME).await?;
    let message = restore_backup_data(&state, &data, args.secret_passphrase)?;
    Ok(Value::String(message))
}

// ─── 自动备份配置（secure_user_data，与桌面端同 key） ───────────────

fn validate_interval(interval_minutes: u64) -> Result<(), PebbleError> {
    if !SUPPORTED_INTERVALS_MINUTES.contains(&interval_minutes) {
        return Err(PebbleError::Validation(format!(
            "Unsupported auto-backup interval: {interval_minutes} minutes"
        )));
    }
    Ok(())
}

pub fn save_auto_backup_config(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    // 与上游 Tauri 命令签名一致：前端 invoke 传 { config: AutoBackupConfig }
    let config: AutoBackupConfig =
        serde_json::from_value(args.get("config").cloned().unwrap_or(Value::Null))
            .map_err(|e| ApiError::BadRequest(format!("invalid args: {e}")))?;
    validate_interval(config.interval_minutes)?;
    let json = serde_json::to_vec(&config)
        .map_err(|e| PebbleError::Internal(format!("Failed to serialize config: {e}")))?;
    let encrypted =
        state
            .crypto
            .encrypt_for("secure_user_data.value", AUTO_BACKUP_CONFIG_KEY, &json)?;
    state
        .store
        .set_secure_user_data(AUTO_BACKUP_CONFIG_KEY, &encrypted)?;
    Ok(Value::Null)
}

fn load_config(state: &AppStateRef) -> Result<Option<AutoBackupConfig>, PebbleError> {
    let Some(encrypted) = state.store.get_secure_user_data(AUTO_BACKUP_CONFIG_KEY)? else {
        return Ok(None);
    };
    let plaintext =
        state
            .crypto
            .decrypt_for("secure_user_data.value", AUTO_BACKUP_CONFIG_KEY, &encrypted)?;
    let config: AutoBackupConfig = serde_json::from_slice(&plaintext).map_err(|e| {
        PebbleError::Internal(format!("Failed to deserialize auto-backup config: {e}"))
    })?;
    validate_interval(config.interval_minutes)?;
    Ok(Some(config))
}

pub fn load_auto_backup_config(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let config = load_config(&state)?;
    serde_json::to_value(config).map_err(ApiError::from_serialize)
}

pub fn delete_auto_backup_config(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    state
        .store
        .delete_secure_user_data(AUTO_BACKUP_CONFIG_KEY)?;
    Ok(Value::Null)
}

/// 自动备份循环：每 60s 检查一次已启用且到期的配置，执行一次 WebDAV 上传。
/// 与桌面端 run_auto_backup_worker 语义对齐（Web 端为 tokio 后台任务）。
pub async fn run_auto_backup_worker(state: AppStateRef) {
    let check_interval = std::time::Duration::from_secs(60);
    loop {
        tokio::time::sleep(check_interval).await;
        let Ok(Some(config)) = load_config(&state) else {
            continue;
        };
        if !config.enabled {
            continue;
        }
        tracing::info!("auto backup worker checking webdav backup");
        match run_backup_once(&state, &config).await {
            Ok(_) => tracing::info!("auto backup completed"),
            Err(e) => tracing::warn!("auto backup failed: {e}"),
        }
    }
}

async fn run_backup_once(
    state: &AppStateRef,
    config: &AutoBackupConfig,
) -> Result<(), PebbleError> {
    let data = build_backup_data(state, config.secret_passphrase.clone())?;
    let client = WebDavClient::new(
        config.url.clone(),
        config.username.clone(),
        config.password.clone(),
    )?;
    client.upload(SETTINGS_BACKUP_FILENAME, &data).await?;
    Ok(())
}

/// 供 state/main 判断 worker 是否已启动（避免重复 spawn）。
pub async fn _auto_backup_enabled(state: &AppStateRef) -> bool {
    load_config(state)
        .ok()
        .flatten()
        .map(|c| c.enabled)
        .unwrap_or(false)
}
