use std::env;

/// 应用配置
#[derive(Debug, Clone)]
pub struct Config {
    pub server_host: String,
    pub server_port: u16,
    pub db_path: String,
    pub bark_api_url: String,
    pub bark_sound: Option<String>,
    pub bark_volume: u8,
    pub bark_group: String,
    pub bark_call: bool,
    pub eew_websocket_url: String,
    pub reconnect_min_seconds: u64,
    pub reconnect_max_seconds: u64,
    pub push_updates: bool,
    pub update_min_report_gap: u32,
    pub ignore_training: bool,
    pub ignore_cancel: bool,
    pub p_wave_km_s: f64,
    pub s_wave_km_s: f64,
    pub stale_origin_seconds: i64,
    pub dedup_keep_minutes: u64,
    pub max_distance_km: f64,
    /// 并发推送的最大数量
    pub max_concurrent_notifications: usize,
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
            bark_sound: env::var("BARK_SOUND")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            bark_volume: env::var("BARK_VOLUME")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(10),
            bark_group: env::var("BARK_GROUP").unwrap_or_else(|_| "地震预警".to_string()),
            bark_call: env::var("BARK_CALL")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(false),
            eew_websocket_url: env::var("EEW_WEBSOCKET_URL")
                .unwrap_or_else(|_| "wss://ws-api.wolfx.jp/all_eew".to_string()),
            reconnect_min_seconds: env::var("RECONNECT_MIN_SECONDS")
                .unwrap_or_else(|_| "1".to_string())
                .parse()
                .unwrap_or(1),
            reconnect_max_seconds: env::var("RECONNECT_MAX_SECONDS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .unwrap_or(30),
            push_updates: env::var("PUSH_UPDATES")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(false),
            update_min_report_gap: env::var("UPDATE_MIN_REPORT_GAP")
                .unwrap_or_else(|_| "1".to_string())
                .parse()
                .unwrap_or(1),
            ignore_training: env::var("IGNORE_TRAINING")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(true),
            ignore_cancel: env::var("IGNORE_CANCEL")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(true),
            p_wave_km_s: env::var("P_WAVE_KM_S")
                .unwrap_or_else(|_| "6.0".to_string())
                .parse()
                .unwrap_or(6.0),
            s_wave_km_s: env::var("S_WAVE_KM_S")
                .unwrap_or_else(|_| "3.5".to_string())
                .parse()
                .unwrap_or(3.5),
            stale_origin_seconds: env::var("STALE_ORIGIN_SECONDS")
                .unwrap_or_else(|_| "600".to_string())
                .parse()
                .unwrap_or(600),
            dedup_keep_minutes: env::var("DEDUP_KEEP_MINUTES")
                .unwrap_or_else(|_| "120".to_string())
                .parse()
                .unwrap_or(120),
            max_distance_km: env::var("MAX_DISTANCE_KM")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .unwrap_or(1000.0),
            max_concurrent_notifications: env::var("MAX_CONCURRENT_NOTIFICATIONS")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .unwrap_or(1000),
            http_pool_size: env::var("HTTP_POOL_SIZE")
                .unwrap_or_else(|_| "200".to_string())
                .parse()
                .unwrap_or(200),
        }
    }
}
