use crate::state::AppState;
use pebble_core::Result;
use pebble_crypto::CryptoService;
use pebble_store::Store;
use std::sync::Arc;
use tokio::sync::OwnedMutexGuard;

pub(crate) const ACCOUNT_AUTH_DATA_PURPOSE: &str = "accounts.auth_data";
pub(crate) const SECURE_USER_DATA_PURPOSE: &str = "secure_user_data.value";
pub(crate) const TRANSLATE_CONFIG_PURPOSE: &str = "translate_config.config";
pub(crate) const ACTIVE_TRANSLATE_CONFIG_ID: &str = "active";

pub(crate) async fn lock_secure_user_data_key(state: &AppState, key: &str) -> OwnedMutexGuard<()> {
    let key_lock = {
        let mut locks = state.secure_user_data_locks.lock().await;
        Arc::clone(
            locks
                .entry(key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    key_lock.lock_owned().await
}

pub(crate) fn encrypt_account_auth_data(
    crypto: &CryptoService,
    account_id: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    crypto.encrypt_for(ACCOUNT_AUTH_DATA_PURPOSE, account_id, plaintext)
}

pub(crate) fn decrypt_account_auth_data(
    crypto: &CryptoService,
    store: &Store,
    account_id: &str,
    encrypted: &[u8],
) -> Result<Vec<u8>> {
    let needs_migration = CryptoService::ciphertext_needs_migration(encrypted);
    let plaintext = crypto.decrypt_for(ACCOUNT_AUTH_DATA_PURPOSE, account_id, encrypted)?;
    if needs_migration {
        let replacement = encrypt_account_auth_data(crypto, account_id, &plaintext)?;
        store.compare_exchange_auth_data(account_id, encrypted, &replacement)?;
    }
    Ok(plaintext)
}

pub(crate) fn load_account_auth_data(
    crypto: &CryptoService,
    store: &Store,
    account_id: &str,
) -> Result<Option<Vec<u8>>> {
    let Some(encrypted) = store.get_auth_data(account_id)? else {
        return Ok(None);
    };
    decrypt_account_auth_data(crypto, store, account_id, &encrypted).map(Some)
}

pub(crate) fn store_account_auth_data(
    crypto: &CryptoService,
    store: &Store,
    account_id: &str,
    plaintext: &[u8],
) -> Result<()> {
    let encrypted = encrypt_account_auth_data(crypto, account_id, plaintext)?;
    store.set_auth_data(account_id, &encrypted)
}

pub(crate) fn encrypt_secure_user_data(
    crypto: &CryptoService,
    key: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    crypto.encrypt_for(SECURE_USER_DATA_PURPOSE, key, plaintext)
}

pub(crate) fn decrypt_secure_user_data(
    crypto: &CryptoService,
    store: &Store,
    key: &str,
    encrypted: &[u8],
) -> Result<Vec<u8>> {
    let needs_migration = CryptoService::ciphertext_needs_migration(encrypted);
    let plaintext = crypto.decrypt_for(SECURE_USER_DATA_PURPOSE, key, encrypted)?;
    if needs_migration {
        let replacement = encrypt_secure_user_data(crypto, key, &plaintext)?;
        store.compare_exchange_secure_user_data(key, encrypted, &replacement)?;
    }
    Ok(plaintext)
}

pub(crate) fn load_secure_user_data(
    crypto: &CryptoService,
    store: &Store,
    key: &str,
) -> Result<Option<Vec<u8>>> {
    let Some(encrypted) = store.get_secure_user_data(key)? else {
        return Ok(None);
    };
    decrypt_secure_user_data(crypto, store, key, &encrypted).map(Some)
}

pub(crate) fn store_secure_user_data(
    crypto: &CryptoService,
    store: &Store,
    key: &str,
    plaintext: &[u8],
) -> Result<()> {
    let encrypted = encrypt_secure_user_data(crypto, key, plaintext)?;
    store.set_secure_user_data(key, &encrypted)
}
