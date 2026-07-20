use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::env;
use std::fmt;
use std::path::PathBuf;
use url::{Host, Url};
use zeroize::Zeroizing;

const DEFAULT_DB_PATH: &str = "./data/disaster-alert.fjall";
const LEGACY_DEFAULT_DB_PATH: &str = "./data/disaster-alert.db";
pub(crate) const BARK_API_BASE_URL: &str = "https://api.day.app";

/// Load configuration values from `.env` in the current working directory.
/// Existing process environment variables take precedence.
pub(crate) fn load_dotenv() -> Result<Option<PathBuf>> {
    let path = PathBuf::from(".env");
    match dotenvy::from_path(&path) {
        Ok(()) => Ok(Some(path)),
        Err(error) if error.not_found() => Ok(None),
        Err(error) => Err(error).context("failed to read .env"),
    }
}

/// 应用配置
#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) instance_terms_accepted: bool,
    pub(crate) server_host: String,
    pub(crate) server_port: u16,
    pub(crate) shutdown_timeout_seconds: u64,
    pub(crate) allowed_origins: Vec<String>,
    pub(crate) db_path: String,
    pub(crate) data_encryption_key: SecretBytes,
    pub(crate) bark_sound: Option<String>,
    pub(crate) bark_volume: u8,
    pub(crate) bark_group: String,
    pub(crate) bark_call: bool,
    pub(crate) alert_detail_base_url: String,
    pub(crate) alert_signing_key: SecretString,
    pub(crate) incident_retention_days: u64,
    pub(crate) delivery_ledger_retention_days: u64,
    pub(crate) operation_retention_days: u64,
    pub(crate) notification_context_retention_days: u64,
    pub(crate) wolfx_websocket_url: String,
    pub(crate) fanstudio_websocket_url: String,
    pub(crate) reconnect_min_seconds: u64,
    pub(crate) reconnect_max_seconds: u64,
    pub(crate) push_updates: bool,
    pub(crate) update_min_report_gap: u32,
    pub(crate) ignore_training: bool,
    pub(crate) ignore_cancel: bool,
    pub(crate) p_wave_km_s: f64,
    pub(crate) s_wave_km_s: f64,
    pub(crate) stale_origin_seconds: i64,
    /// 并发推送的最大数量
    pub(crate) max_concurrent_notifications: usize,
    /// HTTP 连接池大小
    pub(crate) http_pool_size: usize,
    pub(crate) reverse_geocoding_enabled: bool,
    pub(crate) reverse_geocoding_url: String,
}

impl Config {
    /// 从环境变量加载配置
    pub(crate) fn from_env() -> Result<Self> {
        let adaptive_concurrency = std::thread::available_parallelism()
            .map_or(32, |threads| threads.get().saturating_mul(16))
            .clamp(16, 256);
        let config = Self {
            instance_terms_accepted: env_bool("INSTANCE_TERMS_ACCEPTED", false)?,
            server_host: env_string("SERVER_HOST", "0.0.0.0"),
            server_port: env_parse("SERVER_PORT", 30010)?,
            shutdown_timeout_seconds: env_parse("SHUTDOWN_TIMEOUT_SECONDS", 15)?,
            allowed_origins: env_list("ALLOWED_ORIGINS"),
            db_path: configured_db_path()?,
            data_encryption_key: required_env_key("DATA_ENCRYPTION_KEY")?,
            bark_sound: env::var("BARK_SOUND")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            bark_volume: env_parse("BARK_VOLUME", 10)?,
            bark_group: env_string("BARK_GROUP", "灾害预警"),
            bark_call: env_bool("BARK_CALL", true)?,
            alert_detail_base_url: required_env_string("ALERT_DETAIL_BASE_URL")?,
            alert_signing_key: required_env_secret("ALERT_SIGNING_KEY")?,
            incident_retention_days: env_parse("INCIDENT_RETENTION_DAYS", 180)?,
            delivery_ledger_retention_days: env_parse("DELIVERY_LEDGER_RETENTION_DAYS", 180)?,
            operation_retention_days: env_parse("OPERATION_RETENTION_DAYS", 7)?,
            notification_context_retention_days: env_parse(
                "NOTIFICATION_CONTEXT_RETENTION_DAYS",
                365,
            )?,
            wolfx_websocket_url: env_string("WOLFX_WEBSOCKET_URL", "wss://ws-api.wolfx.jp/all_eew"),
            fanstudio_websocket_url: env_string(
                "FANSTUDIO_WEBSOCKET_URL",
                "wss://ws.fanstudio.tech/all",
            ),
            reconnect_min_seconds: env_parse("RECONNECT_MIN_SECONDS", 1)?,
            reconnect_max_seconds: env_parse("RECONNECT_MAX_SECONDS", 30)?,
            push_updates: env_bool("PUSH_UPDATES", false)?,
            update_min_report_gap: env_parse("UPDATE_MIN_REPORT_GAP", 1)?,
            ignore_training: env_bool("IGNORE_TRAINING", true)?,
            ignore_cancel: env_bool("IGNORE_CANCEL", false)?,
            p_wave_km_s: env_parse("P_WAVE_KM_S", 6.0)?,
            s_wave_km_s: env_parse("S_WAVE_KM_S", 3.5)?,
            stale_origin_seconds: env_parse("STALE_ORIGIN_SECONDS", 600)?,
            max_concurrent_notifications: env_parse(
                "MAX_CONCURRENT_NOTIFICATIONS",
                adaptive_concurrency,
            )?,
            http_pool_size: env_parse("HTTP_POOL_SIZE", adaptive_concurrency)?,
            reverse_geocoding_enabled: env_bool("REVERSE_GEOCODING_ENABLED", true)?,
            reverse_geocoding_url: env_string(
                "REVERSE_GEOCODING_URL",
                "https://nominatim.openstreetmap.org/reverse",
            ),
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        validate_websocket_url("WOLFX_WEBSOCKET_URL", &self.wolfx_websocket_url, None)?;
        validate_websocket_url(
            "FANSTUDIO_WEBSOCKET_URL",
            &self.fanstudio_websocket_url,
            Some("/all"),
        )?;
        if self.reconnect_min_seconds == 0 {
            bail!("RECONNECT_MIN_SECONDS must be greater than 0");
        }
        if self.shutdown_timeout_seconds == 0 || self.shutdown_timeout_seconds > 300 {
            bail!("SHUTDOWN_TIMEOUT_SECONDS must be in 1..=300");
        }
        if self.db_path.trim().is_empty() {
            bail!("DB_PATH must not be empty");
        }
        if self.reconnect_min_seconds > self.reconnect_max_seconds {
            bail!("RECONNECT_MIN_SECONDS must be <= RECONNECT_MAX_SECONDS");
        }
        if !(self.p_wave_km_s.is_finite() && self.p_wave_km_s > 0.0) {
            bail!("P_WAVE_KM_S must be a finite positive number");
        }
        if !(self.s_wave_km_s.is_finite() && self.s_wave_km_s > 0.0) {
            bail!("S_WAVE_KM_S must be a finite positive number");
        }
        if self.stale_origin_seconds < 0 {
            bail!("STALE_ORIGIN_SECONDS must be >= 0");
        }
        if self.max_concurrent_notifications == 0 || self.max_concurrent_notifications > 10_000 {
            bail!("MAX_CONCURRENT_NOTIFICATIONS must be in 1..=10000");
        }
        if self.http_pool_size == 0 || self.http_pool_size > 10_000 {
            bail!("HTTP_POOL_SIZE must be in 1..=10000");
        }
        if self.bark_volume > 10 {
            bail!("BARK_VOLUME must be in 0..=10");
        }
        if self.bark_group.chars().count() > 80 {
            bail!("BARK_GROUP must contain at most 80 characters");
        }
        if self.bark_sound.as_ref().is_some_and(|sound| {
            sound.is_empty()
                || sound.len() > 64
                || !sound
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        }) {
            bail!("BARK_SOUND must contain 1..=64 URL-safe ASCII characters");
        }
        validate_public_base_url("ALERT_DETAIL_BASE_URL", &self.alert_detail_base_url)?;
        if self.incident_retention_days == 0 || self.incident_retention_days > 3_650 {
            bail!("INCIDENT_RETENTION_DAYS must be in 1..=3650");
        }
        if self.delivery_ledger_retention_days == 0 || self.delivery_ledger_retention_days > 3_650 {
            bail!("DELIVERY_LEDGER_RETENTION_DAYS must be in 1..=3650");
        }
        if self.delivery_ledger_retention_days < self.incident_retention_days {
            bail!("DELIVERY_LEDGER_RETENTION_DAYS must be >= INCIDENT_RETENTION_DAYS");
        }
        if self.operation_retention_days == 0 || self.operation_retention_days > 365 {
            bail!("OPERATION_RETENTION_DAYS must be in 1..=365");
        }
        if self.notification_context_retention_days == 0
            || self.notification_context_retention_days > 3_650
        {
            bail!("NOTIFICATION_CONTEXT_RETENTION_DAYS must be in 1..=3650");
        }
        if self.reverse_geocoding_enabled {
            validate_http_url("REVERSE_GEOCODING_URL", &self.reverse_geocoding_url)?;
        }
        Ok(())
    }
}

pub(crate) struct SecretString(Zeroizing<String>);

pub(crate) struct SecretBytes(Zeroizing<[u8; 32]>);

impl SecretBytes {
    pub(crate) fn expose(&self) -> [u8; 32] {
        *self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl SecretString {
    pub(crate) fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

fn validate_public_base_url(name: &str, value: &str) -> Result<()> {
    if value.len() > 512 {
        bail!("{name} must contain at most 512 bytes");
    }
    let parsed = Url::parse(value).with_context(|| format!("invalid {name}"))?;
    let secure_scheme = parsed.scheme() == "https"
        || parsed.scheme() == "http" && parsed.host().is_some_and(host_is_loopback);
    if !secure_scheme
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!(
            "{name} must use HTTPS (HTTP is allowed only for loopback) and contain no credentials, query, or fragment"
        );
    }
    Ok(())
}

fn host_is_loopback(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
}

fn validate_http_url(name: &str, value: &str) -> Result<()> {
    let parsed = Url::parse(value).with_context(|| format!("invalid {name}"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("{name} must be an HTTP(S) URL without credentials, query, or fragment");
    }
    Ok(())
}

fn validate_websocket_url(name: &str, value: &str, required_path: Option<&str>) -> Result<()> {
    let parsed = Url::parse(value).with_context(|| format!("invalid {name}"))?;
    if !matches!(parsed.scheme(), "ws" | "wss")
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("{name} must be a WS(S) URL without credentials, query, or fragment");
    }
    if required_path.is_some_and(|path| parsed.path() != path) {
        bail!("{name} must use the /all endpoint");
    }
    Ok(())
}

pub(crate) fn normalize_bark_url(value: &str) -> Result<String> {
    let parsed = Url::parse(value.trim()).context("must be an absolute URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("must be an HTTP(S) URL without credentials, query, or fragment");
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

fn env_string(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

fn configured_db_path() -> Result<String> {
    match env::var("DB_PATH") {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => {
            if !std::path::Path::new(DEFAULT_DB_PATH).exists()
                && std::path::Path::new(LEGACY_DEFAULT_DB_PATH).exists()
            {
                bail!(
                    "legacy database {LEGACY_DEFAULT_DB_PATH} exists while the Fjall target \
                     {DEFAULT_DB_PATH} does not; run disaster-alert-migrate and set DB_PATH to \
                     the generated Fjall directory"
                );
            }
            Ok(DEFAULT_DB_PATH.to_string())
        }
        Err(error) => Err(error).context("failed to read DB_PATH"),
    }
}

fn required_env_string(name: &str) -> Result<String> {
    let value = env::var(name).with_context(|| format!("{name} is required"))?;
    let value = value.trim().to_string();
    if value.is_empty() {
        bail!("{name} cannot be empty");
    }
    Ok(value)
}

fn required_env_secret(name: &str) -> Result<SecretString> {
    let mut value = Zeroizing::new(env::var(name).with_context(|| format!("{name} is required"))?);
    let trimmed = Zeroizing::new(value.trim().to_string());
    value.clear();
    if trimmed.is_empty() {
        bail!("{name} cannot be empty");
    }
    Ok(SecretString(trimmed))
}

fn required_env_key(name: &str) -> Result<SecretBytes> {
    let encoded = Zeroizing::new(required_env_string(name)?);
    parse_key(name, &encoded)
}

fn parse_key(name: &str, encoded: &str) -> Result<SecretBytes> {
    let decoded = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(encoded.as_bytes())
            .with_context(|| format!("{name} must be URL-safe base64 without padding"))?,
    );
    anyhow::ensure!(
        decoded.len() == 32,
        "{name} must decode to exactly 32 bytes"
    );
    let mut key = Zeroizing::new([0_u8; 32]);
    key.copy_from_slice(&decoded);
    Ok(SecretBytes(key))
}

fn env_list(name: &str) -> Vec<String> {
    env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn env_parse<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr + Copy,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match env::var(name) {
        Ok(value) => value
            .trim()
            .parse::<T>()
            .with_context(|| format!("failed to parse {name}={value:?}")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).with_context(|| format!("failed to read {name}")),
    }
}

fn env_bool(name: &str, default: bool) -> Result<bool> {
    match env::var(name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => bail!("failed to parse {name}={value:?} as boolean"),
        },
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).with_context(|| format!("failed to read {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_bark_url, parse_key, validate_public_base_url};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    #[test]
    fn database_encryption_key_requires_exactly_32_bytes() -> anyhow::Result<()> {
        let encoded = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        anyhow::ensure!(parse_key("TEST_KEY", &encoded)?.expose() == [7; 32]);
        anyhow::ensure!(parse_key("TEST_KEY", &URL_SAFE_NO_PAD.encode([7_u8; 31])).is_err());
        anyhow::ensure!(parse_key("TEST_KEY", "not base64!").is_err());
        Ok(())
    }

    #[test]
    fn normalizes_supported_bark_urls() -> anyhow::Result<()> {
        anyhow::ensure!(normalize_bark_url(" https://api.day.app/ ")? == "https://api.day.app");
        anyhow::ensure!(
            normalize_bark_url("https://BARK.EXAMPLE.COM")? == "https://bark.example.com"
        );
        anyhow::ensure!(
            normalize_bark_url("http://192.168.1.10:8080/")? == "http://192.168.1.10:8080"
        );
        anyhow::ensure!(normalize_bark_url("http://[::1]:8080/")? == "http://[::1]:8080");
        anyhow::ensure!(
            normalize_bark_url("https://example.com/bark///")? == "https://example.com/bark"
        );
        Ok(())
    }

    #[test]
    fn rejects_unsafe_or_unsupported_urls() {
        for value in [
            "https://api.day.app@evil.example",
            "https://api.day.app?target=localhost",
            "https://api.day.app/#fragment",
            "ftp://api.day.app",
            "not-a-url",
        ] {
            assert!(normalize_bark_url(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn public_http_urls_accept_all_ip_loopback_forms_only() {
        assert!(validate_public_base_url("TEST_URL", "http://127.0.0.2:8080").is_ok());
        assert!(validate_public_base_url("TEST_URL", "http://[::1]:8080").is_ok());
        assert!(validate_public_base_url("TEST_URL", "http://localhost:8080").is_ok());
        assert!(validate_public_base_url("TEST_URL", "http://192.168.1.1:8080").is_err());
        assert!(validate_public_base_url("TEST_URL", "http://example.com").is_err());
    }
}
