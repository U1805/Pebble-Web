use std::path::PathBuf;

/// 服务配置，全部来自环境变量（与部署态 compose 变量名一致）。
///
/// 环境变量命名是本项目的唯一事实来源，禁止再设同义别名。
/// 部分字段供阶段四（auth/store）与阶段七（static）启用，暂未读取。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    /// 监听端口（PEBBLE_PORT，默认 8080）
    pub port: u16,
    /// 数据目录（PEBBLE_DATA_DIR，容器内 /data 由 volume 挂载）
    pub data_dir: PathBuf,
    /// Web 用户登录密码（PEBBLE_PASSWORD，argon2 校验，阶段四起生效）
    pub password: Option<String>,
    /// JWT 签名密钥（PEBBLE_JWT_SECRET，至少 32 字符，阶段四起生效）
    pub jwt_secret: Option<String>,
    /// 邮件同步间隔秒（PEBBLE_SYNC_INTERVAL，默认 300）
    pub sync_interval: u64,
    /// 凭据加密密钥（PEBBLE_ENCRYPTION_KEY，hex 32 字节，阶段四起生效）
    pub encryption_key: Option<String>,
    /// 前端静态目录（PEBBLE_STATIC_DIR，默认 dist；阶段七接入）
    pub static_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Self {
        let data_dir = std::env::var("PEBBLE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data"));
        let static_dir = std::env::var("PEBBLE_STATIC_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./dist"));

        let config = Config {
            port: std::env::var("PEBBLE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8080),
            data_dir,
            password: std::env::var("PEBBLE_PASSWORD").ok(),
            jwt_secret: std::env::var("PEBBLE_JWT_SECRET").ok(),
            sync_interval: std::env::var("PEBBLE_SYNC_INTERVAL")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            encryption_key: std::env::var("PEBBLE_ENCRYPTION_KEY").ok(),
            static_dir,
        };

        if config.jwt_secret.is_none() {
            tracing::warn!("PEBBLE_JWT_SECRET 未设置：登录与命令鉴权将在后续阶段启用，当前仅健康检查可用");
        }
        config
    }
}