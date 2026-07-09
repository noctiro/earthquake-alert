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

/// Bark 推送客户端，负责重试和无效订阅清理
#[derive(Clone)]
pub struct BarkNotifier {
    api_url: String,
    client: reqwest::Client,
    subscription_store: SubscriptionStore,
    push_config: BarkPushConfig,
}

impl BarkNotifier {
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

        tracing::info!(
            event = "bark.initialized",
            api_url = %api_url.trim_end_matches('/'),
            pool_size,
            "bark.initialized"
        );
        Ok(Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            client,
            subscription_store,
            push_config,
        })
    }

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
            lines.push("[测试] 这是一条模拟预警，不是真实地震".to_string());
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

    pub async fn send_subscription_confirm(&self, subscription: &Subscription) -> Result<()> {
        let title = "地震预警订阅成功";
        let subtitle = if subscription.locations.len() > 1 {
            format!("已保存 {} 个监测地点", subscription.locations.len())
        } else if subscription.location_name.trim().is_empty() {
            "已保存监测地点".to_string()
        } else {
            format!("已保存 {}", subscription.location_name.trim())
        };
        let mut lines = vec!["你将按当前通知级别规则接收地震预警".to_string()];
        for location in subscription.normalized_locations() {
            let name = if location.name.trim().is_empty() {
                "未命名地点"
            } else {
                location.name.trim()
            };
            lines.push(format!(
                "{}: {:.4}, {:.4}",
                name, location.latitude, location.longitude
            ));
        }
        let body = lines.join("\n");

        self.send_notification(&subscription.bark_id, "active", title, &subtitle, &body)
            .await
    }

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

        let mut retries = 0;
        let max_retries = 2;

        loop {
            match self.client.get(&url).send().await {
                Ok(response) => {
                    let status = response.status();

                    if status.is_success() {
                        tracing::debug!(
                            event = "bark.push_succeeded",
                            bark_id = %mask_bark_id(bark_id),
                            status = status.as_u16(),
                            "bark.push_succeeded"
                        );
                        return Ok(());
                    } else {
                        let status_code = status.as_u16();
                        let error_text = response.text().await.unwrap_or_default();

                        // Bark 对无效 key 可能返回 500；这些状态继续重试意义不大，直接清理订阅
                        if status_code == 400 || status_code == 404 || status_code == 500 {
                            tracing::warn!(
                                event = "bark.push_rejected",
                                bark_id = %mask_bark_id(bark_id),
                                status = status_code,
                                response_body = %error_text,
                                cleanup = true,
                                "bark.push_rejected"
                            );

                            if let Err(e) = self.subscription_store.delete_subscription(bark_id) {
                                tracing::error!(
                                    event = "subscription.cleanup_failed",
                                    bark_id = %mask_bark_id(bark_id),
                                    error = ?e,
                                    "subscription.cleanup_failed"
                                );
                            } else {
                                tracing::info!(
                                    event = "subscription.cleaned_up",
                                    bark_id = %mask_bark_id(bark_id),
                                    reason = "bark_rejected",
                                    "subscription.cleaned_up"
                                );
                            }

                            return Err(anyhow::anyhow!(
                                "Bark 推送失败 (HTTP {}), 已删除订阅",
                                status_code
                            ));
                        }

                        if retries < max_retries {
                            retries += 1;
                            tracing::warn!(
                                event = "bark.push_retrying",
                                bark_id = %mask_bark_id(bark_id),
                                retry = retries,
                                max_retries,
                                status = status.as_u16(),
                                response_body = %error_text,
                                "bark.push_retrying"
                            );
                            tokio::time::sleep(Duration::from_millis(100 * retries)).await;
                            continue;
                        }

                        tracing::error!(
                            event = "bark.push_failed",
                            bark_id = %mask_bark_id(bark_id),
                            status = status.as_u16(),
                            response_body = %error_text,
                            "bark.push_failed"
                        );
                        return Err(anyhow::anyhow!("Bark 推送失败: {}", status));
                    }
                }
                Err(e) => {
                    if retries < max_retries {
                        retries += 1;
                        tracing::warn!(
                            event = "bark.request_retrying",
                            bark_id = %mask_bark_id(bark_id),
                            retry = retries,
                            max_retries,
                            error = ?e,
                            "bark.request_retrying"
                        );
                        tokio::time::sleep(Duration::from_millis(100 * retries)).await;
                        continue;
                    }

                    tracing::error!(
                        event = "bark.request_failed",
                        bark_id = %mask_bark_id(bark_id),
                        error = ?e,
                        "bark.request_failed"
                    );
                    return Err(e.into());
                }
            }
        }
    }
}
