use crate::delivery::message::{AlertTiming, format_disaster_alert};
use crate::models::{DisasterEvent, MonitoringTarget, Subscription, mask_device_key};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_TITLE_CHARS: usize = 180;
const MAX_SUBTITLE_CHARS: usize = 180;
const MAX_BODY_CHARS: usize = 4_000;
const MAX_BARK_PAYLOAD_BYTES: usize = 3_800;

#[derive(Debug)]
pub(crate) enum BarkDeliveryError {
    Transient(anyhow::Error),
    Permanent(anyhow::Error),
}

impl BarkDeliveryError {
    pub(crate) const fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }

    pub(crate) fn transient(error: impl Into<anyhow::Error>) -> Self {
        Self::Transient(error.into())
    }

    pub(crate) fn permanent(error: impl Into<anyhow::Error>) -> Self {
        Self::Permanent(error.into())
    }
}

impl std::fmt::Display for BarkDeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient(error) | Self::Permanent(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for BarkDeliveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transient(error) | Self::Permanent(error) => error.source(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BarkPushConfig {
    sound: Option<String>,
    volume: u8,
    group: String,
    call: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AlertRecipient<'a> {
    bark_url: &'a str,
    device_key: &'a str,
    target: &'a MonitoringTarget,
}

#[derive(Clone)]
pub(crate) struct CountdownRecipient {
    bark_url: String,
    device_key: String,
    target: MonitoringTarget,
}

impl<'a> AlertRecipient<'a> {
    pub(crate) fn new(subscription: &'a Subscription, target: &'a MonitoringTarget) -> Self {
        Self {
            bark_url: subscription.bark_base_url(),
            device_key: subscription.device_key(),
            target,
        }
    }

    pub(crate) fn to_countdown_recipient(&self) -> CountdownRecipient {
        CountdownRecipient {
            bark_url: self.bark_url.to_string(),
            device_key: self.device_key.to_string(),
            target: self.target.clone(),
        }
    }
}

struct BarkMessage<'a> {
    bark_url: &'a str,
    device_key: &'a str,
    level: &'a str,
    title: &'a str,
    subtitle: &'a str,
    body: &'a str,
    detail_url: Option<&'a str>,
    use_alert_sound: bool,
}

/// Bark 推送客户端，负责受限并发的可靠投递。
#[derive(Clone)]
pub(crate) struct BarkNotifier {
    allowed_urls: Arc<Vec<String>>,
    client: reqwest::Client,
    push_config: BarkPushConfig,
    concurrency: Arc<Semaphore>,
}

pub(crate) struct BarkPermit {
    _permit: OwnedSemaphorePermit,
}

impl BarkNotifier {
    pub(crate) fn new(
        allowed_urls: Vec<String>,
        pool_size: usize,
        max_concurrent: usize,
        push_config: BarkPushConfig,
    ) -> Result<Self> {
        push_config.validate()?;
        anyhow::ensure!(
            !allowed_urls.is_empty(),
            "Bark URL allowlist cannot be empty"
        );
        let client = reqwest::Client::builder()
            .user_agent("DisasterAlert/1.0")
            .timeout(Duration::from_secs(3))
            .connect_timeout(Duration::from_secs(3))
            .pool_max_idle_per_host(pool_size)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .http2_adaptive_window(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        tracing::info!(
            event = "bark.initialized",
            allowed_url_count = allowed_urls.len(),
            pool_size,
            "bark.initialized"
        );
        Ok(Self {
            allowed_urls: Arc::new(allowed_urls),
            client,
            push_config,
            concurrency: Arc::new(Semaphore::new(max_concurrent.max(1))),
        })
    }

    pub(crate) fn allows_bark_url(&self, bark_url: &str) -> bool {
        self.allowed_urls.iter().any(|allowed| allowed == bark_url)
    }

    pub(crate) fn allowed_bark_urls(&self) -> Vec<String> {
        self.allowed_urls.as_ref().clone()
    }

    pub(crate) async fn send_disaster_alert(
        &self,
        recipient: &AlertRecipient<'_>,
        level: &str,
        event: &DisasterEvent,
        timing: Option<&AlertTiming>,
        detail_url: &str,
    ) -> std::result::Result<(), BarkDeliveryError> {
        self.send_disaster_alert_inner(recipient, level, event, timing, detail_url, true)
            .await
    }

    async fn send_disaster_alert_inner(
        &self,
        recipient: &AlertRecipient<'_>,
        level: &str,
        event: &DisasterEvent,
        timing: Option<&AlertTiming>,
        detail_url: &str,
        use_alert_sound: bool,
    ) -> std::result::Result<(), BarkDeliveryError> {
        let content = format_disaster_alert(event, recipient.target, timing, current_epoch_ms());
        let title = truncate_chars(&content.title, MAX_TITLE_CHARS);
        let subtitle = truncate_chars(&content.subtitle, MAX_SUBTITLE_CHARS);
        let body = truncate_chars(&content.body, MAX_BODY_CHARS);
        self.send_notification(BarkMessage {
            bark_url: recipient.bark_url,
            device_key: recipient.device_key,
            level,
            title: &title,
            subtitle: &subtitle,
            body: &body,
            detail_url: Some(detail_url),
            use_alert_sound,
        })
        .await
    }

    pub(crate) async fn send_disaster_countdown(
        &self,
        recipient: &CountdownRecipient,
        event: &DisasterEvent,
        timing: &AlertTiming,
        detail_url: &str,
    ) -> std::result::Result<(), BarkDeliveryError> {
        let content =
            format_disaster_alert(event, &recipient.target, Some(timing), current_epoch_ms());
        let title = truncate_chars(&content.title, MAX_TITLE_CHARS);
        let subtitle = truncate_chars(&content.subtitle, MAX_SUBTITLE_CHARS);
        let body = truncate_chars(&content.body, MAX_BODY_CHARS);
        self.send_notification(BarkMessage {
            bark_url: &recipient.bark_url,
            device_key: &recipient.device_key,
            level: "passive",
            title: &title,
            subtitle: &subtitle,
            body: &body,
            detail_url: Some(detail_url),
            use_alert_sound: false,
        })
        .await
    }

    pub(crate) async fn acquire_permit(
        &self,
    ) -> std::result::Result<BarkPermit, BarkDeliveryError> {
        self.concurrency
            .clone()
            .acquire_owned()
            .await
            .map(|permit| BarkPermit { _permit: permit })
            .context("Bark delivery concurrency limiter closed")
            .map_err(BarkDeliveryError::transient)
    }

    pub(crate) fn try_acquire_permit(
        &self,
    ) -> std::result::Result<Option<BarkPermit>, BarkDeliveryError> {
        match self.concurrency.clone().try_acquire_owned() {
            Ok(permit) => Ok(Some(BarkPermit { _permit: permit })),
            Err(tokio::sync::TryAcquireError::NoPermits) => Ok(None),
            Err(error) => Err(BarkDeliveryError::transient(anyhow::anyhow!(
                "Bark delivery concurrency limiter closed: {error}"
            ))),
        }
    }

    pub(crate) async fn send_subscription_confirm_with_permit(
        &self,
        subscription: &Subscription,
        permit: BarkPermit,
    ) -> std::result::Result<(), BarkDeliveryError> {
        let title = "灾害预警接收测试";
        let (subtitle, body) = subscription_confirmation_summary(subscription);

        self.send_notification_with_permit(
            BarkMessage {
                bark_url: subscription.bark_base_url(),
                device_key: subscription.device_key(),
                level: "timeSensitive",
                title,
                subtitle: &subtitle,
                body: &body,
                detail_url: None,
                use_alert_sound: false,
            },
            Some(permit),
        )
        .await
    }

    pub(crate) async fn send_test_notification(
        &self,
        bark_url: &str,
        device_key: &str,
        level: &str,
    ) -> std::result::Result<(), BarkDeliveryError> {
        let level = normalize_test_level(level);
        let title = "灾害预警测试通知";
        let subtitle = match level {
            "critical" => "Critical 测试 · 会按关键通知参数发送",
            "active" => "Active 测试 · 普通横幅提醒",
            "passive" => "Passive 测试 · 仅进入通知中心",
            _ => "Time Sensitive 测试 · 订阅通道正常",
        };
        let body = "这是一条手动测试消息，不代表真实灾害事件。收到后即可确认 Bark Key 和服务端推送链路可用。";
        self.send_notification(BarkMessage {
            bark_url,
            device_key,
            level,
            title,
            subtitle,
            body,
            detail_url: None,
            use_alert_sound: level == "critical",
        })
        .await
    }

    async fn send_notification(
        &self,
        message: BarkMessage<'_>,
    ) -> std::result::Result<(), BarkDeliveryError> {
        self.send_notification_with_permit(message, None).await
    }

    async fn send_notification_with_permit(
        &self,
        message: BarkMessage<'_>,
        permit: Option<BarkPermit>,
    ) -> std::result::Result<(), BarkDeliveryError> {
        let level = normalize_bark_level(message.level);
        let payload = fitted_bark_payload(&message, &self.push_config, level)
            .map_err(BarkDeliveryError::permanent)?;
        let BarkMessage {
            bark_url,
            device_key,
            level: _,
            title: _,
            subtitle: _,
            body: _,
            detail_url: _,
            use_alert_sound: _,
        } = message;
        if !self.allows_bark_url(bark_url) {
            return Err(BarkDeliveryError::permanent(anyhow::anyhow!(
                "订阅使用的 Bark URL 已被管理员停用，请重新配置"
            )));
        }
        let url = format!("{bark_url}/push");

        let _permit = match permit {
            Some(permit) => permit,
            None => self.acquire_permit().await?,
        };
        let response = match self.client.post(&url).json(&payload).send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::error!(
                    event = "bark.request_failed",
                    device_key = %mask_device_key(device_key),
                    error = ?error,
                    "bark.request_failed"
                );
                return Err(BarkDeliveryError::transient(error));
            }
        };
        let status = response.status();
        let status_code = status.as_u16();
        let body_text = limited_response_text(response).await.map_err(|error| {
            if status.is_success() || bark_failure_is_transient(status) {
                BarkDeliveryError::transient(error)
            } else {
                BarkDeliveryError::permanent(error)
            }
        })?;
        let outcome = classify_bark_response(status, &body_text);
        if outcome.is_ok() {
            tracing::debug!(
                event = "bark.push_succeeded",
                device_key = %mask_device_key(device_key),
                status = status_code,
                "bark.push_succeeded"
            );
            return Ok(());
        }
        if status.is_client_error() || status.is_success() {
            tracing::warn!(
                event = "bark.push_rejected",
                device_key = %mask_device_key(device_key),
                status = status_code,
                cleanup = false,
                "bark.push_rejected"
            );
        } else {
            tracing::error!(
                event = "bark.push_failed",
                device_key = %mask_device_key(device_key),
                status = status_code,
                "bark.push_failed"
            );
        }
        outcome
    }
}

fn bark_failure_is_transient(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

fn current_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn subscription_confirmation_summary(subscription: &Subscription) -> (String, String) {
    let target_names = subscription
        .targets
        .iter()
        .map(|target| {
            let name = target.label.trim();
            if name.is_empty() {
                "未命名地点"
            } else {
                name
            }
        })
        .collect::<Vec<_>>();
    let category_names = subscription
        .alerts
        .iter()
        .map(|alert| alert.category().label())
        .collect::<Vec<_>>();
    let subtitle = format!(
        "Bark 通知通道正常 · {} 个地点 · {} 类预警",
        target_names.len(),
        category_names.len()
    );
    let body = format!(
        "监测地点：{}\n预警类型：{}\n请返回网页查看订阅保存结果。",
        target_names.join("、"),
        category_names.join("、")
    );
    (subtitle, body)
}

fn normalize_bark_level(level: &str) -> &'static str {
    match level.trim().to_ascii_lowercase().as_str() {
        "passive" => "passive",
        "active" => "active",
        "timesensitive" => "timeSensitive",
        "critical" => "critical",
        _ => "critical",
    }
}

fn normalize_test_level(level: &str) -> &'static str {
    match level.trim().to_ascii_lowercase().as_str() {
        "passive" => "passive",
        "active" => "active",
        "critical" => "critical",
        "timesensitive" | "time_sensitive" | "time-sensitive" => "timeSensitive",
        _ => "timeSensitive",
    }
}

fn bark_payload(
    message: &BarkMessage<'_>,
    push_config: &BarkPushConfig,
    level: &str,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "device_key": message.device_key,
        "title": message.title,
        "subtitle": message.subtitle,
        "body": message.body,
        "group": push_config.group,
        "level": level,
    });
    if let Some(detail_url) = message.detail_url {
        payload["url"] = serde_json::json!(detail_url);
    }
    if level != "passive" && message.use_alert_sound {
        payload["volume"] = serde_json::json!(push_config.volume);
        if push_config.call {
            payload["call"] = serde_json::json!("1");
        }
        if let Some(sound) = &push_config.sound {
            payload["sound"] = serde_json::json!(sound);
        }
    }
    payload
}

fn fitted_bark_payload(
    message: &BarkMessage<'_>,
    push_config: &BarkPushConfig,
    level: &str,
) -> Result<serde_json::Value> {
    let mut payload = bark_payload(message, push_config, level);
    if serde_json::to_vec(&payload)?.len() <= MAX_BARK_PAYLOAD_BYTES {
        return Ok(payload);
    }

    for (field, value) in [
        ("body", message.body),
        ("subtitle", message.subtitle),
        ("title", message.title),
    ] {
        fit_payload_field(&mut payload, field, value)?;
        if serde_json::to_vec(&payload)?.len() <= MAX_BARK_PAYLOAD_BYTES {
            return Ok(payload);
        }
    }
    anyhow::bail!("Bark detail URL leaves no room for a valid push payload")
}

fn fit_payload_field(payload: &mut serde_json::Value, field: &str, value: &str) -> Result<()> {
    let mut low = 0usize;
    let mut high = value.len();
    let mut best = None;
    while low <= high {
        let middle = low + (high - low) / 2;
        let body = truncate_utf8_bytes_with_ellipsis(value, middle);
        payload[field] = serde_json::json!(body);
        if serde_json::to_vec(&payload)?.len() <= MAX_BARK_PAYLOAD_BYTES {
            best = Some(payload[field].clone());
            low = middle.saturating_add(1);
        } else if middle == 0 {
            break;
        } else {
            high = middle - 1;
        }
    }
    payload[field] = best.unwrap_or_else(|| serde_json::json!(""));
    Ok(())
}

fn truncate_utf8_bytes_with_ellipsis(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let ellipsis = "…";
    if max_bytes < ellipsis.len() {
        return String::new();
    }
    let mut end = (max_bytes - ellipsis.len()).min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut output = String::with_capacity(end + ellipsis.len());
    output.push_str(&value[..end]);
    output.push_str(ellipsis);
    output
}

impl BarkPushConfig {
    #[must_use]
    pub(crate) fn new(sound: Option<String>, volume: u8, group: String, call: bool) -> Self {
        Self {
            sound,
            volume,
            group,
            call,
        }
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.volume <= 10, "BARK_VOLUME must be in 0..=10");
        Ok(())
    }
}

fn classify_bark_response(
    status: reqwest::StatusCode,
    body: &str,
) -> std::result::Result<(), BarkDeliveryError> {
    if !status.is_success() {
        let detail = bark_response_detail(body);
        let error = anyhow::anyhow!("Bark push failed: HTTP {}{detail}", status.as_u16());
        return if bark_failure_is_transient(status) {
            Err(BarkDeliveryError::transient(error))
        } else {
            Err(BarkDeliveryError::permanent(error))
        };
    }
    if body.trim().is_empty() {
        return Ok(());
    }

    #[derive(Deserialize)]
    struct BarkEnvelope {
        code: Option<i64>,
        success: Option<bool>,
        message: Option<String>,
    }

    let response = serde_json::from_str::<BarkEnvelope>(body).map_err(|error| {
        BarkDeliveryError::transient(
            anyhow::Error::new(error).context("Bark push returned an invalid application response"),
        )
    })?;
    let detail = response
        .message
        .as_deref()
        .map(sanitize_provider_message)
        .filter(|message| !message.is_empty())
        .map_or_else(String::new, |message| format!(": {message}"));
    match response.code {
        Some(200) if response.success != Some(false) => Ok(()),
        None if response.success == Some(true) => Ok(()),
        Some(429) => Err(BarkDeliveryError::transient(anyhow::anyhow!(
            "Bark push failed: application code 429{detail}"
        ))),
        Some(code) if (500..=599).contains(&code) => Err(BarkDeliveryError::transient(
            anyhow::anyhow!("Bark push failed: application code {code}{detail}"),
        )),
        Some(code) if (400..=499).contains(&code) => Err(BarkDeliveryError::permanent(
            anyhow::anyhow!("Bark push failed: application code {code}{detail}"),
        )),
        Some(code) => Err(BarkDeliveryError::transient(anyhow::anyhow!(
            "Bark push returned contradictory fields: code {code}, success {:?}{detail}",
            response.success
        ))),
        None if response.success == Some(false) => Err(BarkDeliveryError::permanent(
            anyhow::anyhow!("Bark push was rejected{detail}"),
        )),
        None => Err(BarkDeliveryError::transient(anyhow::anyhow!(
            "Bark push response omitted both code and success"
        ))),
    }
}

fn bark_response_detail(body: &str) -> String {
    #[derive(Deserialize)]
    struct ErrorEnvelope {
        code: Option<i64>,
        message: Option<String>,
    }
    let Ok(response) = serde_json::from_str::<ErrorEnvelope>(body) else {
        return String::new();
    };
    let mut parts = Vec::with_capacity(2);
    if let Some(code) = response.code {
        parts.push(format!("application code {code}"));
    }
    if let Some(message) = response.message {
        let message = sanitize_provider_message(&message);
        if !message.is_empty() {
            parts.push(message);
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(": "))
    }
}

fn sanitize_provider_message(message: &str) -> String {
    message
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect::<String>()
        .trim()
        .to_string()
}

async fn limited_response_text(mut response: reqwest::Response) -> Result<String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        anyhow::bail!("Bark response exceeded size limit");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read Bark response")?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            anyhow::bail!("Bark response exceeded size limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value
        .chars()
        .take(max_chars.saturating_sub(1))
        .chain(['…'])
        .collect()
}

#[cfg(test)]
mod tests {
    use anyhow::Context as _;

    use super::{
        AlertRecipient, AlertTiming, BarkMessage, BarkNotifier, BarkPushConfig,
        MAX_BARK_PAYLOAD_BYTES, bark_failure_is_transient, bark_payload, classify_bark_response,
        current_epoch_ms, fitted_bark_payload, normalize_bark_level,
        subscription_confirmation_summary, truncate_chars, truncate_utf8_bytes_with_ellipsis,
    };
    use crate::models::{
        AlertRule, DisasterCategory, DisasterEvent, GeoPoint, MonitoringTarget,
        NotificationDestination, ProviderChannel, Subscription,
    };

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let truncated = truncate_chars("灾害预警abcdef", 6);
        assert_eq!(truncated, "灾害预警a…");
        assert_eq!(truncated.chars().count(), 6);

        assert_eq!(truncate_utf8_bytes_with_ellipsis("灾害abcdef", 8), "灾…");
        assert_eq!(truncate_utf8_bytes_with_ellipsis("灾害abcdef", 0), "");
        assert_eq!(truncate_utf8_bytes_with_ellipsis("灾害abcdef", 1), "");
        assert_eq!(truncate_utf8_bytes_with_ellipsis("灾害abcdef", 2), "");
    }

    #[test]
    fn only_retryable_http_statuses_are_classified_as_transient() {
        assert!(bark_failure_is_transient(
            reqwest::StatusCode::REQUEST_TIMEOUT
        ));
        assert!(bark_failure_is_transient(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(bark_failure_is_transient(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!bark_failure_is_transient(reqwest::StatusCode::BAD_REQUEST));
        assert!(!bark_failure_is_transient(
            reqwest::StatusCode::UNAUTHORIZED
        ));
        assert!(!bark_failure_is_transient(reqwest::StatusCode::OK));
    }

    #[test]
    fn application_status_controls_delivery_outcome() {
        assert!(classify_bark_response(reqwest::StatusCode::OK, r#"{"code":200}"#).is_ok());
        assert!(matches!(
            classify_bark_response(reqwest::StatusCode::OK, r#"{"code":503,"message":"busy"}"#),
            Err(super::BarkDeliveryError::Transient(_))
        ));
        assert!(matches!(
            classify_bark_response(
                reqwest::StatusCode::OK,
                r#"{"code":400,"success":true,"message":"bad key"}"#
            ),
            Err(super::BarkDeliveryError::Permanent(_))
        ));
        assert!(
            classify_bark_response(reqwest::StatusCode::OK, r#"{"code":200,"success":false}"#)
                .is_err()
        );
    }

    #[test]
    fn subscription_confirmation_is_clear_without_coordinates() {
        let subscription = Subscription::new(
            NotificationDestination::Bark {
                base_url: "https://api.day.app".to_string(),
                device_key: "abc123".to_string(),
            },
            vec![MonitoringTarget {
                label: "东京".to_string(),
                point: GeoPoint {
                    latitude: 35.6,
                    longitude: 139.6,
                },
                region: crate::models::AdministrativeRegion::default(),
            }],
            vec![AlertRule::default_for(DisasterCategory::EarthquakeWarning)],
        );

        let (subtitle, body) = subscription_confirmation_summary(&subscription);

        assert_eq!(subtitle, "Bark 通知通道正常 · 1 个地点 · 1 类预警");
        assert!(body.contains("监测地点：东京"));
        assert!(body.contains("预警类型：地震预警"));
        assert!(!body.contains("35.6"));
        assert!(!body.contains("139.6"));
    }

    #[test]
    fn subscription_confirmation_requests_a_banner_without_repeated_ringing() {
        let message = BarkMessage {
            bark_url: "https://api.day.app",
            device_key: "abc123",
            level: "timeSensitive",
            title: "灾害预警接收测试",
            subtitle: "接收测试成功",
            body: "订阅配置正在保存",
            detail_url: None,
            use_alert_sound: false,
        };
        let config = BarkPushConfig {
            sound: Some("alarm".to_string()),
            volume: 10,
            group: "灾害预警".to_string(),
            call: true,
        };
        let level = normalize_bark_level(message.level);

        let payload = bark_payload(&message, &config, level);

        assert_eq!(payload["level"], "timeSensitive");
        assert!(payload.get("sound").is_none());
        assert!(payload.get("volume").is_none());
        assert!(payload.get("call").is_none());
    }

    #[test]
    fn disaster_alerts_keep_the_configured_alert_sound() {
        let message = BarkMessage {
            bark_url: "https://api.day.app",
            device_key: "abc123",
            level: "critical",
            title: "地震预警",
            subtitle: "接收测试",
            body: "测试内容",
            detail_url: Some("https://alert.example.com/incidents/test"),
            use_alert_sound: true,
        };
        let config = BarkPushConfig {
            sound: Some("alarm".to_string()),
            volume: 10,
            group: "灾害预警".to_string(),
            call: true,
        };

        let payload = bark_payload(&message, &config, normalize_bark_level(message.level));

        assert_eq!(payload["level"], "critical");
        assert_eq!(payload["sound"], "alarm");
        assert_eq!(payload["volume"], 10);
        assert_eq!(payload["call"], "1");
        assert_eq!(payload["url"], "https://alert.example.com/incidents/test");
    }

    #[tokio::test]
    async fn countdown_pushes_do_not_repeat_the_alert_sound() -> anyhow::Result<()> {
        async fn capture(
            axum::extract::State(sender): axum::extract::State<
                tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
            >,
            axum::Json(payload): axum::Json<serde_json::Value>,
        ) -> axum::Json<serde_json::Value> {
            let _sent = sender.send(payload);
            axum::Json(serde_json::json!({ "code": 200 }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let app = axum::Router::new()
            .route("/push", axum::routing::post(capture))
            .with_state(sender);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let base_url = format!("http://{address}");
        let subscription = Subscription::new(
            NotificationDestination::Bark {
                base_url: base_url.clone(),
                device_key: "abc123".to_string(),
            },
            vec![MonitoringTarget {
                label: "上海家中".to_string(),
                point: GeoPoint {
                    latitude: 31.2,
                    longitude: 121.5,
                },
                region: crate::models::AdministrativeRegion::default(),
            }],
            vec![AlertRule::default_for(DisasterCategory::EarthquakeWarning)],
        );
        let event = DisasterEvent {
            category: DisasterCategory::EarthquakeWarning,
            channel: ProviderChannel::FanStudio,
            source: "fanstudio.eew".to_string(),
            event_id: "event".to_string(),
            revision: "1".to_string(),
            report_num: 1,
            title: "地震预警 四川泸定".to_string(),
            description: String::new(),
            latitude: Some(29.6),
            longitude: Some(102.1),
            magnitude: Some(6.2),
            depth_km: Some(10.0),
            affected_regions: Vec::new(),
            radius_km: None,
            level: 3,
            occurred_at: "2026-07-14T10:20:30+08:00".to_string(),
            final_report: false,
            cancel: false,
            training: false,
        };
        let now_ms = current_epoch_ms();
        let timing = AlertTiming {
            distance_km: 82.0,
            hypocentral_km: 83.0,
            estimated_intensity: 3.2,
            p_arrival_at_ms: now_ms.saturating_add(2_000),
            s_arrival_at_ms: now_ms.saturating_add(5_000),
        };
        let notifier = BarkNotifier::new(
            vec![base_url],
            2,
            2,
            BarkPushConfig::new(Some("alarm".to_string()), 10, "灾害预警".to_string(), true),
        )?;
        let recipient = AlertRecipient::new(&subscription, &subscription.targets[0]);
        notifier
            .send_disaster_alert(
                &recipient,
                "critical",
                &event,
                Some(&timing),
                "https://alerts.example.test/detail",
            )
            .await?;
        notifier
            .send_disaster_countdown(
                &recipient.to_countdown_recipient(),
                &event,
                &timing,
                "https://alerts.example.test/detail",
            )
            .await?;

        let initial = receiver
            .recv()
            .await
            .context("missing initial Bark payload")?;
        let countdown = receiver
            .recv()
            .await
            .context("missing countdown Bark payload")?;
        anyhow::ensure!(
            initial["title"]
                .as_str()
                .is_some_and(|title| title.starts_with("地震播报 ") && title.ends_with("秒后到达"))
        );
        anyhow::ensure!(initial["sound"] == "alarm");
        anyhow::ensure!(initial["volume"] == 10);
        anyhow::ensure!(initial["call"] == "1");
        anyhow::ensure!(countdown.get("sound").is_none());
        anyhow::ensure!(countdown.get("volume").is_none());
        anyhow::ensure!(countdown.get("call").is_none());
        anyhow::ensure!(countdown["level"] == "passive");
        server.abort();
        Ok(())
    }

    #[test]
    fn long_signed_detail_url_is_preserved_by_truncating_display_fields() -> anyhow::Result<()> {
        let detail_url = format!(
            "https://alert.example.com/incidents/{}/notifications/{}",
            "a".repeat(22),
            "b".repeat(2_850)
        );
        let title = "标题".repeat(180);
        let subtitle = "副标题".repeat(180);
        let body = "内容".repeat(4_000);
        let message = BarkMessage {
            bark_url: "https://api.day.app",
            device_key: "abc123",
            level: "critical",
            title: &title,
            subtitle: &subtitle,
            body: &body,
            detail_url: Some(&detail_url),
            use_alert_sound: true,
        };
        let config = BarkPushConfig {
            sound: Some("alarm".to_string()),
            volume: 10,
            group: "灾害预警".repeat(20),
            call: true,
        };

        let payload = fitted_bark_payload(&message, &config, "critical")?;

        anyhow::ensure!(payload["url"] == detail_url);
        anyhow::ensure!(serde_json::to_vec(&payload)?.len() <= MAX_BARK_PAYLOAD_BYTES);
        anyhow::ensure!(
            payload["body"]
                .as_str()
                .is_some_and(|value| value.len() < body.len())
        );
        Ok(())
    }
}
