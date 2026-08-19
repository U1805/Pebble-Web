use pebble_crypto::CryptoService;
use pebble_store::Store;

/// 与桌面端一致的 auth_data 加密用途（src-tauri/commands/encrypted_store.rs）。
/// 保持同一 purpose 字符串，Web 与桌面端读写同一数据目录时凭据互兼容。
pub const ACCOUNT_AUTH_DATA_PURPOSE: &str = "accounts.auth_data";

/// 加密账户凭据并落库（AAD 绑定 account_id）。
pub fn store_account_auth_data(
    crypto: &CryptoService,
    store: &Store,
    account_id: &str,
    plaintext: &[u8],
) -> Result<(), String> {
    let encrypted = crypto
        .encrypt_for(ACCOUNT_AUTH_DATA_PURPOSE, account_id, plaintext)
        .map_err(|e| e.to_string())?;
    store
        .set_auth_data(account_id, &encrypted)
        .map_err(|e| e.to_string())
}

/// 读取并解密账户凭据。decrypt_for 兼容桌面端历史遗留的无作用域密文（自动迁移语义）。
pub fn load_account_auth_data(
    crypto: &CryptoService,
    store: &Store,
    account_id: &str,
) -> Result<Option<Vec<u8>>, String> {
    let Some(encrypted) = store
        .get_auth_data(account_id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let plaintext = crypto
        .decrypt_for(ACCOUNT_AUTH_DATA_PURPOSE, account_id, &encrypted)
        .map_err(|e| e.to_string())?;
    Ok(Some(plaintext))
}

/// 清除账户凭据（删除账户时调用）。
pub fn clear_account_auth_data(store: &Store, account_id: &str) -> Result<(), String> {
    store
        .clear_auth_data(account_id)
        .map_err(|e| e.to_string())
}