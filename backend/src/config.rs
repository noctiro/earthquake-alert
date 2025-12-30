use std::env;

/// 应用配置
#[derive(Debug, Clone)]
pub struct Config {
    pub server_host: String,
    pub server_port: u16,
    pub db_path: String,
    pub bark_api_url: String,
    /// 并发推送的最大数量
    pub max_concurrent_notifications: usize,
    /// 每批处理的订阅数量
    pub batch_size: usize,
    /// HTTP 连接池大小
    pub http_pool_size: usize,
}

impl Config {
    /// 从环境变量加载配置
    pub fn from_env() -> Self {
        Self {
            server_host: env::var("SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            server_port: env::var("SERVER_PORT")
                .unwrap_or_else(|_| "30010".to_string())
                .parse()
                .unwrap_or(30010),
            db_path: env::var("DB_PATH").unwrap_or_else(|_| "./data/earthquake.db".to_string()),
            bark_api_url: env::var("BARK_API_URL")
                .unwrap_or_else(|_| "https://api.day.app".to_string()),
            // 并发配置：百万级别并发支持
            max_concurrent_notifications: env::var("MAX_CONCURRENT_NOTIFICATIONS")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .unwrap_or(1000),
            batch_size: env::var("BATCH_SIZE")
                .unwrap_or_else(|_| "5000".to_string())
                .parse()
                .unwrap_or(5000),
            http_pool_size: env::var("HTTP_POOL_SIZE")
                .unwrap_or_else(|_| "200".to_string())
                .parse()
                .unwrap_or(200),
        }
    }
}
