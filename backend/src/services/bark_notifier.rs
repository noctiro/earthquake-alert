use crate::db::SubscriptionStore;
use crate::models::{CommonEarthquakeInfo, Subscription, mask_bark_id};
use anyhow::Result;
use std::time::Duration;
use urlencoding::encode;

#[derive(Debug, Clone)]
pub struct BarkPushConfig {
    pub sound: Option<String>,
    pub volume: u8,
    pub group: String,
    pub call: bool,
}

#[derive(Debug, Clone)]
pub struct AlertTiming {
    pub distance_km: f64,
    pub hypocentral_km: f64,
    pub estimated_intensity: u8,
    pub seconds_to_p: i64,
    pub seconds_to_s: i64,
}

/// Bark 推送服务（支持高并发）
#[derive(Clone)]
pub struct BarkNotifier {
    api_url: String,
    client: reqwest::Client,
    subscription_store: SubscriptionStore,
    push_config: BarkPushConfig,
}

impl BarkNotifier {
    /// 创建新的 Bark 通知器，支持连接池和高并发
    pub fn new(
        api_url: String,
        pool_size: usize,
        subscription_store: SubscriptionStore,
        push_config: BarkPushConfig,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("EarthquakeAlert/1.0")
            .timeout(Duration::from_secs(3))
            .connect_timeout(Duration::from_secs(3))
            .pool_max_idle_per_host(pool_size)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .http2_adaptive_window(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .build()?;

        tracing::info!("初始化 Bark 通知器，连接池大小: {}", pool_size);
        Ok(Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            client,
            subscription_store,
            push_config,
        })
    }

    /// 发送地震预警通知
    pub async fn send_earthquake_alert(
        &self,
        subscription: &Subscription,
        level: &str,
        earthquake: &CommonEarthquakeInfo,
        timing: &AlertTiming,
    ) -> Result<()> {
        let eta = if timing.seconds_to_s > 0 {
            format!("{}秒后到达", timing.seconds_to_s)
        } else {
            "已到达".to_string()
        };

        let prefix = if earthquake.training {
            "地震预警测试"
        } else {
            "地震预警"
        };
        let title = format!("{} {}", prefix, eta);

        let subtitle = format!(
            "M{:.1} 预计烈度{} 距{:.0}km",
            earthquake.magnitude, timing.estimated_intensity, timing.distance_km
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

        let report_label = if earthquake.report_num > 0 {
            format!(" 第{}报", earthquake.report_num)
        } else {
            String::new()
        };
        let status_label = if earthquake.final_report {
            " 终报"
        } else {
            ""
        };
        let mut lines = Vec::new();
        if earthquake.training {
            lines.push("[测试] 这是一条模拟预警，不是真实地震。".to_string());
        }
        lines.extend([
            format!("地点: {}", region_text),
            format!(
                "震源: {:.2}, {:.2} 深度{:.0}km",
                earthquake.latitude, earthquake.longitude, earthquake.depth
            ),
            format!(
                "距离: 震中{:.0}km 震源{:.0}km",
                timing.distance_km, timing.hypocentral_km
            ),
            format!(
                "预计: P波{:+}秒 S波{:+}秒 烈度{}",
                timing.seconds_to_p, timing.seconds_to_s, timing.estimated_intensity
            ),
            format!(
                "震级: M{:.1} 最大烈度{}",
                earthquake.magnitude, earthquake.max_intensity
            ),
            format!(
                "来源: {}{}{}",
                earthquake.source_type, report_label, status_label
            ),
            format!("发震: {}", earthquake.origin_time),
        ]);
        let body = lines.join("\n");

        self.send_notification(&subscription.bark_id, level, &title, &subtitle, &body)
            .await
    }

    /// 发送 Bark 通知（支持重试）
    async fn send_notification(
        &self,
        bark_id: &str,
        level: &str,
        title: &str,
        subtitle: &str,
        body: &str,
    ) -> Result<()> {
        let level = match level.trim().to_ascii_lowercase().as_str() {
            "passive" => "passive",
            "active" => "active",
            "critical" => "critical",
            _ => "critical",
        };
        let mut params = vec![("group", self.push_config.group.as_str()), ("level", level)];
        let volume = self.push_config.volume.to_string();
        if self.push_config.volume > 0 && level != "passive" {
            params.push(("volume", volume.as_str()));
        }
        if self.push_config.call && level != "passive" {
            params.push(("call", "1"));
        }
        if let Some(sound) = &self.push_config.sound
            && level != "passive"
        {
            params.push(("sound", sound.as_str()));
        }

        let query = params
            .iter()
            .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
            .collect::<Vec<_>>()
            .join("&");

        let url = format!(
            "{}/{}/{}/{}/{}?{}",
            self.api_url,
            encode(bark_id),
            encode(title),
            encode(subtitle),
            encode(body),
            query
        );

        // 带重试的发送逻辑
        let mut retries = 0;
        let max_retries = 2;

        loop {
            match self.client.get(&url).send().await {
                Ok(response) => {
                    let status = response.status();

                    if status.is_success() {
                        tracing::debug!("Bark 推送成功: {}", mask_bark_id(bark_id));
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
                                mask_bark_id(bark_id)
                            );

                            // 删除该订阅
                            if let Err(e) = self.subscription_store.delete_subscription(bark_id) {
                                tracing::error!(
                                    "删除订阅失败 ({}): {:?}",
                                    mask_bark_id(bark_id),
                                    e
                                );
                            } else {
                                tracing::info!(
                                    "已自动删除无效的 bark_id: {}",
                                    mask_bark_id(bark_id)
                                );
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
