use std::collections::HashMap;

use pebble_core::PebbleError;
use pebble_crypto::{
    passphrase::{decrypt_with_passphrase, encrypt_with_passphrase, PassphraseEncryptedBlob},
    CryptoService,
};
use pebble_store::cloud_sync::{
    preview_backup, serialize_backup, BackupPreview, BackupSecretSummary, RestoredAuthData,
    RestoredPrivateData, RestoredSecureUserData, SettingsBackup,
};
use pebble_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::credentials;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 与桌面端 encrypted_store.rs 一致的用途常量；SECURE_USER_DATA 复用 credentials.rs 定义。
const TRANSLATE_CONFIG_PURPOSE: &str = "translate_config.config";
const ACTIVE_TRANSLATE_CONFIG_ID: &str = "active";
const KANBAN_CONTEXT_NOTES_KEY: &str = "kanban_context_notes";

/// 备份文件导出参数（后端 snake_case 契约，调用层已由前端转换）。
#[derive(Deserialize)]
pub struct ExportArgs {
    #[serde(default)]
    pub secret_passphrase: Option<String>,
}

#[derive(Deserialize)]
pub struct PreviewArgs {
    pub data: String,
}

#[derive(Deserialize)]
pub struct ImportArgs {
    pub data: String,
    #[serde(default)]
    pub secret_passphrase: Option<String>,
}

/// 与桌面端 BackupSecrets 序列化兼容的私密数据包。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupSecrets {
    #[serde(default)]
    pub account_auth: Vec<AccountAuthBackup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate_config: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountAuthBackup {
    pub account_id: String,
    pub provider: String,
    pub auth_data: Value,
}

fn provider_slug(provider: &pebble_core::ProviderType) -> &'static str {
    match provider {
        pebble_core::ProviderType::Imap => "imap",
        pebble_core::ProviderType::Pop3 => "pop3",
        pebble_core::ProviderType::Gmail => "gmail",
        pebble_core::ProviderType::Outlook => "outlook",
    }
}

fn secret_summary(secrets: &BackupSecrets) -> BackupSecretSummary {
    BackupSecretSummary {
        account_auth_count: secrets.account_auth.len(),
        has_translate_config: secrets.translate_config.is_some(),
    }
}

// ─── secure user data / translate 加解密（供备份打包与恢复） ───────────

fn encrypt_secure_user_data(
    crypto: &CryptoService,
    key: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>, PebbleError> {
    crypto.encrypt_for(credentials::SECURE_USER_DATA_PURPOSE, key, plaintext)
}

fn decrypt_translate_config_ext(
    crypto: &CryptoService,
    stored: &str,
) -> Result<String, PebbleError> {
    // 与桌面端 decrypt_config 语义一致：legacy plaintext JSON 直接当原文，
    // 否则 hex 解码后按目的用途解密（此处不做 in-place 迁移，备份场景无需）。
    if serde_json::from_str::<Value>(stored).is_ok() {
        return Ok(stored.to_string());
    }
    let bytes = hex::decode(stored)
        .map_err(|e| PebbleError::Internal(format!("Invalid translate config hex: {e}")))?;
    let decrypted =
        crypto.decrypt_for(TRANSLATE_CONFIG_PURPOSE, ACTIVE_TRANSLATE_CONFIG_ID, &bytes)?;
    String::from_utf8(decrypted)
        .map_err(|e| PebbleError::Internal(format!("Invalid UTF-8 in translate config: {e}")))
}

fn encrypt_translate_config_ext(
    crypto: &CryptoService,
    plaintext: &str,
) -> Result<String, PebbleError> {
    let encrypted = crypto.encrypt_for(
        TRANSLATE_CONFIG_PURPOSE,
        ACTIVE_TRANSLATE_CONFIG_ID,
        plaintext.as_bytes(),
    )?;
    Ok(hex::encode(encrypted))
}

// ─── 打包私有数据 ──────────────────────────────────────────────────────────

fn collect_backup_secrets(
    store: &Store,
    crypto: &CryptoService,
) -> Result<BackupSecrets, PebbleError> {
    let mut account_auth = Vec::new();
    for account in store.list_accounts()? {
        let Some(decrypted) = credentials::load_account_auth_data(crypto, store, &account.id)
            .map_err(|e| PebbleError::Internal(e))?
        else {
            continue;
        };
        let auth_data: Value = serde_json::from_slice(&decrypted).map_err(|e| {
            PebbleError::Internal(format!(
                "Failed to parse decrypted auth data for {}: {e}",
                account.email
            ))
        })?;
        account_auth.push(AccountAuthBackup {
            account_id: account.id,
            provider: provider_slug(&account.provider).to_string(),
            auth_data,
        });
    }

    let translate_config = store
        .get_translate_config()?
        .map(|tc| decrypt_translate_config_ext(crypto, &tc.config))
        .transpose()?;

    Ok(BackupSecrets {
        account_auth,
        translate_config,
    })
}

fn attach_encrypted_secrets(
    store: &Store,
    crypto: &CryptoService,
    backup: &mut SettingsBackup,
    secret_passphrase: Option<String>,
) -> Result<(), PebbleError> {
    let Some(passphrase) = secret_passphrase else {
        return Ok(());
    };
    let secrets = collect_backup_secrets(store, crypto)?;
    if secrets.account_auth.is_empty() && secrets.translate_config.is_none() {
        return Ok(());
    }
    let plaintext = serde_json::to_vec(&secrets)
        .map_err(|e| PebbleError::Internal(format!("Failed to serialize backup secrets: {e}")))?;
    let encrypted = encrypt_with_passphrase(&plaintext, &passphrase)?;
    backup.secret_summary = Some(secret_summary(&secrets));
    backup.encrypted_secrets = Some(serde_json::to_value(encrypted).map_err(|e| {
        PebbleError::Internal(format!("Failed to serialize encrypted backup secrets: {e}"))
    })?);
    Ok(())
}

fn decrypt_secrets_from_backup(
    backup: &SettingsBackup,
    secret_passphrase: Option<String>,
) -> Result<Option<BackupSecrets>, PebbleError> {
    let Some(value) = &backup.encrypted_secrets else {
        return Ok(None);
    };
    let Some(passphrase) = secret_passphrase else {
        return Err(PebbleError::Validation(
            "This backup contains encrypted account passwords, OAuth tokens, or API keys. Enter the backup encryption password to restore them.".to_string(),
        ));
    };
    let encrypted: PassphraseEncryptedBlob =
        serde_json::from_value(value.clone()).map_err(|e| {
            PebbleError::Validation(format!("Invalid encrypted backup secret payload: {e}"))
        })?;
    let plaintext = decrypt_with_passphrase(&encrypted, &passphrase)?;
    serde_json::from_slice(&plaintext)
        .map(Some)
        .map_err(|e| PebbleError::Validation(format!("Failed to parse backup secrets: {e}")))
}

fn prepare_restored_private_data(
    crypto: &CryptoService,
    backup: &SettingsBackup,
    has_kanban_context_notes: bool,
    secrets: Option<BackupSecrets>,
) -> Result<RestoredPrivateData, PebbleError> {
    let mut private_data = RestoredPrivateData::default();

    if has_kanban_context_notes {
        let notes = serde_json::to_vec(&backup.kanban_context_notes)
            .map_err(|e| PebbleError::Internal(format!("Failed to serialize kanban notes: {e}")))?;
        let encrypted = encrypt_secure_user_data(crypto, KANBAN_CONTEXT_NOTES_KEY, &notes)?;
        private_data.secure_user_data.push(RestoredSecureUserData {
            key: KANBAN_CONTEXT_NOTES_KEY.to_string(),
            encrypted: Some(encrypted),
        });
    }

    let Some(secrets) = secrets else {
        return Ok(private_data);
    };

    for account in secrets.account_auth {
        let auth_bytes = serde_json::to_vec(&account.auth_data).map_err(|e| {
            PebbleError::Internal(format!(
                "Failed to serialize restored auth data for account {}: {e}",
                account.account_id
            ))
        })?;
        let encrypted = crypto.encrypt_for(
            credentials::ACCOUNT_AUTH_DATA_PURPOSE,
            &account.account_id,
            &auth_bytes,
        )?;
        private_data.auth_data.push(RestoredAuthData {
            account_id: account.account_id,
            provider: account.provider,
            encrypted,
        });
    }

    if let (Some(secret_config), Some(mut translate_config)) =
        (secrets.translate_config, backup.translate_config.clone())
    {
        translate_config.config = encrypt_translate_config_ext(crypto, &secret_config)?;
        private_data.translate_config = Some(translate_config);
    }

    Ok(private_data)
}

// ─── 命令 ──────────────────────────────────────────────────────────────────

/// 构建备份字节流（供 export_backup_file / backup_to_webdav 复用）。
pub(crate) fn build_backup_data(
    state: &AppStateRef,
    secret_passphrase: Option<String>,
) -> Result<Vec<u8>, PebbleError> {
    let exported = state.store.export_settings()?;
    let mut backup: SettingsBackup = serde_json::from_slice(&exported)
        .map_err(|e| PebbleError::Internal(format!("Failed to build backup payload: {e}")))?;
    backup.kanban_context_notes = load_kanban_context_notes(&state.store, &state.crypto)?;
    attach_encrypted_secrets(&state.store, &state.crypto, &mut backup, secret_passphrase)?;
    serialize_backup(&backup)
}

/// 从备份字节流恢复设置（供 import_backup_file / restore_from_webdav 复用）。
/// 备份加密密码错误统一归为 Validation（HTTP 400），避免触发 Web 会话登出。
pub(crate) fn restore_backup_data(
    state: &AppStateRef,
    data: &[u8],
    secret_passphrase: Option<String>,
) -> Result<String, PebbleError> {
    let _ = preview_backup(data)?;

    let backup_value: Value = serde_json::from_slice(data)
        .map_err(|e| PebbleError::Validation(format!("Failed to parse backup: {e}")))?;
    let has_kanban_context_notes = backup_value.get("kanban_context_notes").is_some();
    let backup: SettingsBackup = serde_json::from_value(backup_value)
        .map_err(|e| PebbleError::Validation(format!("Failed to parse backup: {e}")))?;

    let backup_secrets = decrypt_secrets_from_backup(&backup, secret_passphrase)
        .map_err(|e| PebbleError::Validation(format!("unable to decrypt backup secrets: {e}")))?;
    let restored_secrets = backup_secrets.is_some();
    let private_data = prepare_restored_private_data(
        &state.crypto,
        &backup,
        has_kanban_context_notes,
        backup_secrets,
    )?;

    state
        .store
        .import_settings_with_private_data(data, private_data)?;
    Ok(if restored_secrets {
        "Settings backup restored with account passwords, OAuth tokens, and API keys."
    } else {
        "Settings backup restored. Reconnect accounts to continue syncing."
    }
    .to_string())
}

/// 导出设置备份（含可选 passphrase 加密的账户凭据/token）。
/// 命令名对齐桌面端 export_backup_file；返回 JSON 字符串，前端浏览器下载落盘。
pub async fn export_backup_file(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: ExportArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid export_backup args: {e}")))?;
    let data = build_backup_data(&state, args.secret_passphrase)?;
    let text = String::from_utf8(data)
        .map_err(|e| PebbleError::Internal(format!("Backup JSON was not valid UTF-8: {e}")))?;
    Ok(Value::String(text))
}

/// 预览备份：返回摘要（版本、数量、是否含加密 secrets），不执行恢复。
/// 命令名对齐桌面端 preview_backup_file。
pub async fn preview_backup_file(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: PreviewArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid preview_backup args: {e}")))?;
    let _ = state; // 纯数据校验，不依赖 state
    let preview: BackupPreview = preview_backup(args.data.as_bytes())?;
    serde_json::to_value(preview).map_err(ApiError::from_serialize)
}

/// 从备份字符串恢复设置（可选恢复加密凭据）。
/// 命令名对齐桌面端 import_backup_file。
pub async fn import_backup_file(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: ImportArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid import_backup args: {e}")))?;
    let message = restore_backup_data(&state, args.data.as_bytes(), args.secret_passphrase)?;
    Ok(Value::String(message))
}

/// 读取 kanban 看板上下文笔记（来自 secure_user_data，DESKTOP 同款）。
fn load_kanban_context_notes(
    store: &Store,
    crypto: &CryptoService,
) -> Result<HashMap<String, String>, PebbleError> {
    let Some(encrypted) = store.get_secure_user_data(KANBAN_CONTEXT_NOTES_KEY)? else {
        return Ok(HashMap::new());
    };
    let decrypted = crypto.decrypt_for(
        credentials::SECURE_USER_DATA_PURPOSE,
        KANBAN_CONTEXT_NOTES_KEY,
        &encrypted,
    )?;
    serde_json::from_slice(&decrypted)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse kanban context notes: {e}")))
}
