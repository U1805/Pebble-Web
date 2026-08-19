use std::path::PathBuf;

/// 服务配置，全部来自环境变量（与部署态 compose 变量名一致）。
///
/// 环境变量命名是本项目的唯一事实来源，禁止再设同义别名。
/// 安全项（PEBBLE_PASSWORD / PEBBLE_JWT_SECRET）为必填并拒绝占位符，
/// 与服务公开到网络的风险匹配（计划书 §68 安全边界）。
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub data_dir: PathBuf,
    pub password_hash: String,
    pub jwt_secret: String,
    pub sync_interval_secs: u64,
    pub static_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let password = std::env::var("PEBBLE_PASSWORD")
            .map_err(|_| "PEBBLE_PASSWORD env var is required".to_string())?;
        let jwt_secret = std::env::var("PEBBLE_JWT_SECRET")
            .map_err(|_| "PEBBLE_JWT_SECRET env var is required".to_string())?;

        if is_insecure_jwt_secret(&jwt_secret) {
            return Err("PEBBLE_JWT_SECRET must be at least 32 chars and not a placeholder"
                .to_string());
        }
        if is_insecure_default_password(&password) {
            return Err("PEBBLE_PASSWORD must be changed from the default value".to_string());
        }

        let port = std::env::var("PEBBLE_PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .map_err(|e| format!("Invalid PEBBLE_PORT: {e}"))?;

        let data_dir =
            PathBuf::from(std::env::var("PEBBLE_DATA_DIR").unwrap_or_else(|_| "/data".to_string()));

        let sync_interval_secs = std::env::var("PEBBLE_SYNC_INTERVAL")
            .unwrap_or_else(|_| "300".to_string())
            .parse::<u64>()
            .map_err(|e| format!("Invalid PEBBLE_SYNC_INTERVAL: {e}"))?;

        let static_dir = PathBuf::from(
            std::env::var("PEBBLE_STATIC_DIR").unwrap_or_else(|_| "./dist".to_string()),
        );

        let password_hash = crate::auth::hash_password(&password)
            .map_err(|e| format!("Failed to hash password: {e}"))?;

        Ok(Self {
            port,
            data_dir,
            password_hash,
            jwt_secret,
            sync_interval_secs,
            static_dir,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("pebble.db")
    }

    pub fn index_dir(&self) -> PathBuf {
        self.data_dir.join("index")
    }

    pub fn attachments_dir(&self) -> PathBuf {
        self.data_dir.join("attachments")
    }
}

fn is_insecure_default_password(password: &str) -> bool {
    matches!(password.trim(), "changeme" | "your-password-here")
}

fn is_insecure_jwt_secret(secret: &str) -> bool {
    let trimmed = secret.trim();
    matches!(
        trimmed,
        "change-this-to-a-random-string"
            | "generate-a-random-string-here"
            | "your-random-secret-at-least-32-chars"
    ) || trimmed.len() < 32
}

#[cfg(test)]
mod tests {
    use super::{is_insecure_default_password, is_insecure_jwt_secret};

    #[test]
    fn rejects_documented_placeholder_passwords() {
        assert!(is_insecure_default_password("changeme"));
        assert!(is_insecure_default_password("your-password-here"));
        assert!(!is_insecure_default_password("correct horse battery staple"));
    }

    #[test]
    fn rejects_placeholder_or_short_jwt_secrets() {
        assert!(is_insecure_jwt_secret("change-this-to-a-random-string"));
        assert!(is_insecure_jwt_secret("your-random-secret-at-least-32-chars"));
        assert!(is_insecure_jwt_secret("short-secret"));
        assert!(!is_insecure_jwt_secret("this-is-a-real-secret-with-32-plus-chars"));
    }
}