use pebble_core::PebbleError;
use pebble_crypto::passphrase::{
    decrypt_with_passphrase, encrypt_with_passphrase, PassphraseEncryptedBlob,
};
use pebble_store::cloud_sync::{
    preview_backup, serialize_backup, BackupPreview, BackupSecretSummary, RestoredAuthData,
    RestoredPrivateData, RestoredSecureUserData, SettingsBackup,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::{encrypted_store, kanban, translate};
use crate::error::ApiError;
use crate::state::AppStateRef;

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

// ─── 打包私有数据 ──────────────────────────────────────────────────────────

fn collect_backup_secrets(state: &AppStateRef) -> Result<BackupSecrets, PebbleError> {
    let store = &state.store;
    let crypto = &state.crypto;
    let mut account_auth = Vec::new();
    for account in store.list_accounts()? {
        let Some(decrypted) = encrypted_store::load_account_auth_data(crypto, store, &account.id)?
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
        .map(|tc| translate::decrypt_config(crypto, store, &tc.config))
        .transpose()?;

    Ok(BackupSecrets {
        account_auth,
        translate_config,
    })
}

fn attach_encrypted_secrets(
    state: &AppStateRef,
    backup: &mut SettingsBackup,
    secret_passphrase: Option<String>,
) -> Result<(), PebbleError> {
    let Some(passphrase) = secret_passphrase else {
        return Ok(());
    };
    let secrets = collect_backup_secrets(state)?;
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
    state: &AppStateRef,
    backup: &SettingsBackup,
    has_kanban_context_notes: bool,
    secrets: Option<BackupSecrets>,
) -> Result<RestoredPrivateData, PebbleError> {
    let mut private_data = RestoredPrivateData::default();

    if has_kanban_context_notes {
        private_data.secure_user_data.push(RestoredSecureUserData {
            key: kanban::KANBAN_CONTEXT_NOTES_KEY.to_string(),
            encrypted: kanban::encrypt_kanban_context_notes_for_state(
                state,
                backup.kanban_context_notes.clone(),
            )?,
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
        let encrypted = encrypted_store::encrypt_account_auth_data(
            &state.crypto,
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
        translate_config.config = translate::encrypt_config(&state.crypto, &secret_config)?;
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
    backup.kanban_context_notes = kanban::load_kanban_context_notes_for_state(state)?;
    attach_encrypted_secrets(state, &mut backup, secret_passphrase)?;
    serialize_backup(&backup)
}

/// 从备份字节流恢复设置（供 import_backup_file / restore_from_webdav 复用）。
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

    let backup_secrets = decrypt_secrets_from_backup(&backup, secret_passphrase)?;
    let restored_secrets = backup_secrets.is_some();
    let private_data =
        prepare_restored_private_data(state, &backup, has_kanban_context_notes, backup_secrets)?;

    state
        .store
        .import_settings_with_private_data(data, private_data)?;
    Ok(if restored_secrets {
        "Settings backup restored with saved credentials. Existing OAuth connections and mailboxes with local data were preserved."
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
