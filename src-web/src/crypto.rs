use std::path::Path;

use pebble_crypto::CryptoService;
use rand::RngCore;

/// DEK（数据加密密钥）管理 —— Web 端独立实现。
///
/// 决策（2026-08-19）：不修改上游 pebble-crypto。上游保留 OS keyring 路径
/// （桌面端 CryptoService::init），Web 端在此按「env → 文件 → 生成落盘」获取
/// 32 字节 DEK，再经 CryptoService::from_key 注入——pebble-crypto 零改动。
pub fn load_or_create_crypto(data_dir: &Path) -> Result<CryptoService, String> {
    if let Some(key) = load_dek_from_env()? {
        return Ok(CryptoService::from_key(key));
    }
    if let Some(key) = load_dek_from_file(&data_dir.join("encryption.key"))? {
        return Ok(CryptoService::from_key(key));
    }

    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    let hex_str = hex::encode(key);

    tracing::info!(
        path = %data_dir.join("encryption.key").display(),
        "no DEK found, generating and persisting new one"
    );
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("Failed to create data dir: {e}"))?;
    // Linux 容器内限制文件权限，避免同机其他用户读到明文密钥
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(data_dir.join("encryption.key"), hex_str.as_bytes())
            .and_then(|_| std::fs::set_permissions(data_dir.join("encryption.key"), std::fs::Permissions::from_mode(0o600)))
            .map_err(|e| format!("Failed to persist DEK: {e}"))?;
    }
    #[cfg(not(unix))]
    std::fs::write(data_dir.join("encryption.key"), hex_str.as_bytes())
        .map_err(|e| format!("Failed to persist DEK: {e}"))?;

    Ok(CryptoService::from_key(key))
}

fn load_dek_from_env() -> Result<Option<[u8; 32]>, String> {
    let Ok(hex_key) = std::env::var("PEBBLE_ENCRYPTION_KEY") else {
        return Ok(None);
    };
    decode_hex_key(&hex_key).map(Some)
}

fn load_dek_from_file(path: &Path) -> Result<Option<[u8; 32]>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read DEK file {}: {e}", path.display()))?;
    decode_hex_key(raw.trim()).map(Some)
}

fn decode_hex_key(hex_key: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(hex_key)
        .map_err(|e| format!("Invalid PEBBLE_ENCRYPTION_KEY hex: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!(
            "PEBBLE_ENCRYPTION_KEY must decode to 32 bytes, got {}",
            bytes.len()
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::decode_hex_key;

    #[test]
    fn decodes_valid_hex_key() {
        let valid = "ab".repeat(32);
        let key = decode_hex_key(&valid).unwrap();
        assert_eq!(key.len(), 32);
        assert_eq!(key[0], 0xab);
    }

    #[test]
    fn rejects_bad_length() {
        assert!(decode_hex_key("00").is_err());
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(decode_hex_key("zz".repeat(32).as_str()).is_err());
    }
}