use crate::db::SubscriptionStore;
use crate::models::{CommonEarthquakeInfo, Subscription};
use anyhow::Result;
use std::time::Duration;

/// Bark 推送服务（支持高并发）
#[derive(Clone)]
pub struct BarkNotifier {
    api_url: String,
    client: reqwest::Client,
    subscription_store: SubscriptionStore,
}

impl BarkNotifier {
    /// 创建新的 Bark 通知器，支持连接池和高并发
    pub fn new(api_url: String, pool_size: usize, subscription_store: SubscriptionStore) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("EarthquakeAlert/1.0")
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(pool_size)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .http2_adaptive_window(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        tracing::info!("初始化 Bark 通知器，连接池大小: {}", pool_size);
        Self {
            api_url,
            client,
            subscription_store,
        }
    }

    /// 发送地震预警通知
    pub async fn send_earthquake_alert(
        &self,
        subscription: &Subscription,
        earthquake: &CommonEarthquakeInfo,
        distance_km: f64,
        estimated_intensity: u8,
    ) -> Result<()> {
        // Title: 简洁有力，显示震级（最显眼）
        let title = format!("地震预警 M{:.1}", earthquake.magnitude);

        // Subtitle: 最关键信息 - 预估震度和距离（次显眼）
        let subtitle = format!(
            "震度 {} 级 · 距离 {:.1} km",
            estimated_intensity, distance_km
        );

        // Body: 详细信息（详细内容）
        let region_text = if earthquake.region.is_empty() {
            format!(
                "{:.2}°N, {:.2}°E",
                earthquake.latitude, earthquake.longitude
            )
        } else {
            earthquake.region.clone()
        };

        let body = format!(
            "震央: {}\n震源深度: {:.0} km\n最大震度: {} 级",
            region_text, earthquake.depth, earthquake.max_intensity,
        );

        self.send_notification(&subscription.bark_id, &title, &subtitle, &body)
            .await
    }

    /// 发送 Bark 通知（支持重试）
    async fn send_notification(
        &self,
        bark_id: &str,
        title: &str,
        subtitle: &str,
        body: &str,
    ) -> Result<()> {
        // URL 编码
        let title_encoded = urlencoding::encode(title);
        let subtitle_encoded = urlencoding::encode(subtitle);
        let body_encoded = urlencoding::encode(body);

        // Bark 推送格式: /:key/:title/:subtitle/:body?params
        let url = format!(
            "{}/{}/{}/{}/{}?group=地震预警&level=critical&volume=5",
            self.api_url,
            urlencoding::encode(bark_id),
            title_encoded,
            subtitle_encoded,
            body_encoded
        );

        // 带重试的发送逻辑
        let mut retries = 0;
        let max_retries = 2;

        loop {
            match self.client.get(&url).send().await {
                Ok(response) => {
                    let status = response.status();

                    if status.is_success() {
                        tracing::debug!("Bark 推送成功: {}", bark_id);
                        return Ok(());
                    } else {
                        let status_code = status.as_u16();
                        let error_text = response.text().await.unwrap_or_default();

                        // 检查是否为需要删除订阅的错误码
                        if status_code == 400 || status_code == 404 || status_code == 500 {
                            tracing::warn!(
                                "Bark 推送失败 (HTTP {}): {} - 删除该 bark_id: {}",
                                status_code,
                                error_text,
                                bark_id
                            );

                            // 删除该订阅
                            if let Err(e) = self.subscription_store.delete_subscription(bark_id) {
                                tracing::error!("删除订阅失败 ({}): {:?}", bark_id, e);
                            } else {
                                tracing::info!("已自动删除无效的 bark_id: {}", bark_id);
                            }

                            return Err(anyhow::anyhow!(
                                "Bark 推送失败 (HTTP {}), 已删除订阅",
                                status_code
                            ));
                        }

                        // 其他错误码，继续重试
                        if retries < max_retries {
                            retries += 1;
                            tracing::warn!(
                                "Bark 推送失败 (重试 {}/{}): {} - {}",
                                retries,
                                max_retries,
                                status,
                                error_text
                            );
                            tokio::time::sleep(Duration::from_millis(100 * retries)).await;
                            continue;
                        }

                        tracing::error!("Bark 推送失败: {} - {}", status, error_text);
                        return Err(anyhow::anyhow!("Bark 推送失败: {}", status));
                    }
                }
                Err(e) => {
                    if retries < max_retries {
                        retries += 1;
                        tracing::warn!("Bark 请求失败 (重试 {}/{}): {:?}", retries, max_retries, e);
                        tokio::time::sleep(Duration::from_millis(100 * retries)).await;
                        continue;
                    }

                    tracing::error!("Bark 请求失败: {:?}", e);
                    return Err(e.into());
                }
            }
        }
    }
}
