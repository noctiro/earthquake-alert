use crate::config::Config;
use crate::db::Database;
use crate::models::{
    CommonEarthquakeInfo, EarthquakeData, Subscription, WebSocketMessage, mask_bark_id,
};
use crate::services::{AlertTiming, BarkNotifier, BarkPushConfig};
use crate::utils::{distance, geohash, intensity};
use anyhow::Result;
use futures::stream::{self, StreamExt};
use futures_util::StreamExt as FuturesStreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Clone)]
struct MonitorConfig {
    websocket_url: String,
    reconnect_min: Duration,
    reconnect_max: Duration,
    push_updates: bool,
    update_min_report_gap: u32,
    ignore_training: bool,
    ignore_cancel: bool,
    p_wave_km_s: f64,
    s_wave_km_s: f64,
    stale_origin_seconds: i64,
    dedup_keep: Duration,
    max_distance_km: f64,
}

#[derive(Clone)]
struct SeenEvent {
    report_num: u32,
    at: Instant,
}

/// 地震监控服务（支持百万级并发）
pub struct EarthquakeMonitor {
    db: Database,
    bark_notifier: BarkNotifier,
    max_concurrent: usize,
    semaphore: Arc<Semaphore>,
    config: MonitorConfig,
    seen_events: Arc<Mutex<HashMap<String, SeenEvent>>>,
}

impl EarthquakeMonitor {
    pub fn new(db: Database, config: Config) -> Result<Self> {
        let subscription_store = db.subscriptions();
        let push_config = BarkPushConfig {
            sound: config.bark_sound.clone(),
            volume: config.bark_volume,
            group: config.bark_group.clone(),
            call: config.bark_call,
        };
        let bark_notifier = BarkNotifier::new(
            config.bark_api_url.clone(),
            config.http_pool_size,
            subscription_store,
            push_config,
        )?;
        let max_concurrent = config.max_concurrent_notifications.max(1);
        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        let monitor_config = MonitorConfig {
            websocket_url: config.eew_websocket_url.clone(),
            reconnect_min: Duration::from_secs(config.reconnect_min_seconds.max(1)),
            reconnect_max: Duration::from_secs(
                config
                    .reconnect_max_seconds
                    .max(config.reconnect_min_seconds.max(1)),
            ),
            push_updates: config.push_updates,
            update_min_report_gap: config.update_min_report_gap.max(1),
            ignore_training: config.ignore_training,
            ignore_cancel: config.ignore_cancel,
            p_wave_km_s: if config.p_wave_km_s > 0.0 {
                config.p_wave_km_s
            } else {
                6.0
            },
            s_wave_km_s: if config.s_wave_km_s > 0.0 {
                config.s_wave_km_s
            } else {
                3.5
            },
            stale_origin_seconds: config.stale_origin_seconds,
            dedup_keep: Duration::from_secs(config.dedup_keep_minutes.max(1) * 60),
            max_distance_km: config.max_distance_km,
        };

        tracing::info!(
            "初始化地震监控服务: 最大并发={}, HTTP连接池={}, websocket={}",
            max_concurrent,
            config.http_pool_size,
            monitor_config.websocket_url
        );

        Ok(Self {
            db,
            bark_notifier,
            max_concurrent,
            semaphore,
            config: monitor_config,
            seen_events: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// 启动监控（会自动重连）
    pub async fn start(&self) -> Result<()> {
        let mut reconnect_delay = self.config.reconnect_min;
        loop {
            tracing::info!("正在连接到地震预警 WebSocket...");

            match self.connect_and_monitor().await {
                Ok(_) => {
                    tracing::warn!("WebSocket 连接正常关闭");
                    reconnect_delay = self.config.reconnect_min;
                }
                Err(e) => {
                    tracing::error!("WebSocket 连接错误: {:?}", e);
                }
            }

            tracing::info!("{}秒后重新连接...", reconnect_delay.as_secs());
            tokio::time::sleep(reconnect_delay).await;
            reconnect_delay = (reconnect_delay * 2).min(self.config.reconnect_max);
        }
    }

    /// 连接并监控 WebSocket
    async fn connect_and_monitor(&self) -> Result<()> {
        let (ws_stream, _) = connect_async(&self.config.websocket_url).await?;
        tracing::info!("WebSocket 已连接到: {}", self.config.websocket_url);

        let (mut _write, mut read) = ws_stream.split();

        // 监听消息
        while let Some(message) = FuturesStreamExt::next(&mut read).await {
            match message {
                Ok(Message::Text(text)) => {
                    if let Err(e) = self.handle_earthquake_message(&text).await {
                        tracing::error!("处理地震消息失败: {:?}", e);
                    }
                }
                Ok(Message::Close(_)) => {
                    tracing::info!("WebSocket 连接关闭");
                    break;
                }
                Ok(Message::Ping(_)) => {
                    tracing::debug!("收到 Ping");
                    // tokio-tungstenite 会自动处理 pong
                }
                Err(e) => {
                    tracing::error!("WebSocket 消息错误: {:?}", e);
                    return Err(e.into());
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// 处理地震消息
    async fn handle_earthquake_message(&self, message: &str) -> Result<()> {
        // 先解析消息类型
        let msg_wrapper: WebSocketMessage = match serde_json::from_str(message) {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!("无法解析消息类型: {:?}, 消息: {}", e, message);
                return Ok(());
            }
        };

        // 过滤掉非地震数据消息
        match msg_wrapper.message_type.as_str() {
            "heartbeat" => {
                tracing::debug!("收到心跳消息");
                return Ok(());
            }
            "pong" => {
                tracing::debug!("收到 pong 消息");
                return Ok(());
            }
            "jma_eqlist" | "cenc_eqlist" => {
                // 地震列表消息，暂不处理
                tracing::debug!("收到地震列表消息: {}", msg_wrapper.message_type);
                return Ok(());
            }
            _ => {
                // 继续处理，可能是地震预警数据
            }
        }

        // 解析地震数据并转换为通用信息
        let common_info = match EarthquakeData::parse_to_common_info(message) {
            Ok(info) => info,
            Err(e) => {
                tracing::error!(
                    "解析地震数据失败 (type={}): {:?}",
                    msg_wrapper.message_type,
                    e
                );
                return Ok(());
            }
        };

        tracing::info!(
            "接收到地震预警 [{}]: id={} report={} M{:.1} 深度{}km @ ({}, {}) - {}",
            common_info.source_type,
            common_info.event_id,
            common_info.report_num,
            common_info.magnitude,
            common_info.depth,
            common_info.latitude,
            common_info.longitude,
            common_info.region
        );

        if self.should_skip_event(&common_info) {
            return Ok(());
        }

        // 查找并推送给相关订阅者
        self.notify_subscribers(&common_info).await?;

        Ok(())
    }

    /// 通知订阅者（并发优化版本，支持百万级并发）
    async fn notify_subscribers(&self, earthquake: &CommonEarthquakeInfo) -> Result<()> {
        let start_time = Instant::now();

        // 1. 计算震央的 GeoHash 及邻居
        let center_geohash = geohash::encode(earthquake.latitude, earthquake.longitude);
        let neighbor_geohashes = geohash::get_neighbors(&center_geohash);

        tracing::info!("检查 {} 个 GeoHash 格子", neighbor_geohashes.len());

        // 2. 获取相关格子内的所有订阅
        let store = self.db.subscriptions();
        let subscriptions = store.get_subscriptions_by_geohashes(&neighbor_geohashes)?;

        let total_candidates = subscriptions.len();
        tracing::info!("找到 {} 个候选订阅", total_candidates);

        // 早期退出：如果没有订阅者，直接返回
        if total_candidates == 0 {
            tracing::info!("没有订阅者，跳过推送");
            return Ok(());
        }

        // 3. 预计算所有订阅者的距离和震度（批处理优化）
        let mut notification_tasks = Vec::with_capacity(total_candidates);

        for subscription in subscriptions {
            if let Some((selected, level, timing)) =
                self.evaluate_subscription(&subscription, earthquake)
            {
                notification_tasks.push((selected, level, timing));
            }
        }

        let tasks_count = notification_tasks.len();
        tracing::info!(
            "需要推送 {} 个通知 (过滤掉 {} 个)",
            tasks_count,
            total_candidates - tasks_count
        );

        if tasks_count == 0 {
            tracing::info!("所有订阅者震度未达阈值，跳过推送");
            return Ok(());
        }

        // 4. 并发发送通知（使用 Semaphore 限制并发数）
        let bark_notifier = self.bark_notifier.clone();
        let semaphore = self.semaphore.clone();
        let earthquake = earthquake.clone();

        let results = stream::iter(notification_tasks)
            .map(|(subscription, level, timing)| {
                let bark_notifier = bark_notifier.clone();
                let semaphore = semaphore.clone();
                let earthquake = earthquake.clone();

                async move {
                    let bark_id = subscription.bark_id.clone();
                    // 获取信号量许可（限流）
                    let permit = semaphore.acquire_owned().await;
                    let permit_guard: OwnedSemaphorePermit = match permit {
                        Ok(permit) => permit,
                        Err(error) => {
                            tracing::error!("推送并发控制已关闭: {:?}", error);
                            return (bark_id, false, None);
                        }
                    };
                    let _permit_guard = permit_guard;

                    tracing::debug!(
                        "推送给 {}: 距离 {:.1}km, 预估震度 {}, 通知级别 {}",
                        mask_bark_id(&bark_id),
                        timing.distance_km,
                        timing.estimated_intensity,
                        level
                    );

                    match bark_notifier
                        .send_earthquake_alert(&subscription, &level, &earthquake, &timing)
                        .await
                    {
                        Ok(_) => (bark_id, true, None),
                        Err(e) => {
                            tracing::error!("推送失败 ({}): {:?}", mask_bark_id(&bark_id), e);
                            (bark_id, false, Some(e))
                        }
                    }
                }
            })
            .buffer_unordered(self.max_concurrent) // 并发执行
            .collect::<Vec<_>>()
            .await;

        // 5. 统计结果
        let notified_count = results.iter().filter(|(_, success, _)| *success).count();
        let error_count = results.iter().filter(|(_, success, _)| !*success).count();

        let elapsed = start_time.elapsed();

        tracing::info!(
            "推送完成: 候选 {} 个, 已推送 {} 个, 失败 {} 个, 耗时 {:.2}s, 平均 {:.0} 个/秒",
            total_candidates,
            notified_count,
            error_count,
            elapsed.as_secs_f64(),
            if elapsed.as_secs_f64() > 0.0 {
                notified_count as f64 / elapsed.as_secs_f64()
            } else {
                0.0
            }
        );

        Ok(())
    }

    fn should_skip_event(&self, earthquake: &CommonEarthquakeInfo) -> bool {
        if earthquake.training && self.config.ignore_training {
            tracing::info!("跳过演练事件: {}", earthquake_key(earthquake));
            return true;
        }
        if earthquake.cancel && self.config.ignore_cancel {
            tracing::info!("跳过取消事件: {}", earthquake_key(earthquake));
            return true;
        }
        if self.config.stale_origin_seconds > 0
            && let Some(age_seconds) = origin_age_seconds(earthquake)
            && age_seconds > self.config.stale_origin_seconds
        {
            tracing::info!(
                "跳过过期事件: {} age={}s",
                earthquake_key(earthquake),
                age_seconds
            );
            return true;
        }

        let mut seen = match self.seen_events.lock() {
            Ok(seen) => seen,
            Err(error) => {
                tracing::error!("事件去重锁已损坏: {:?}", error);
                return true;
            }
        };
        let now = Instant::now();
        seen.retain(|_, value| now.duration_since(value.at) <= self.config.dedup_keep);
        let key = earthquake_key(earthquake);
        if let Some(previous) = seen.get(&key) {
            let is_update = earthquake.report_num > previous.report_num;
            let gap = earthquake.report_num.saturating_sub(previous.report_num);
            if !self.config.push_updates || !is_update || gap < self.config.update_min_report_gap {
                tracing::debug!(
                    "跳过重复事件: {} previous_report={} report={}",
                    key,
                    previous.report_num,
                    earthquake.report_num
                );
                return true;
            }
        }
        seen.insert(
            key,
            SeenEvent {
                report_num: earthquake.report_num,
                at: now,
            },
        );
        false
    }

    fn evaluate_subscription(
        &self,
        subscription: &Subscription,
        earthquake: &CommonEarthquakeInfo,
    ) -> Option<(Subscription, String, AlertTiming)> {
        let mut best: Option<(Subscription, String, AlertTiming)> = None;
        for location in subscription.normalized_locations() {
            let dist = distance::vincenty_distance(
                earthquake.latitude,
                earthquake.longitude,
                location.latitude,
                location.longitude,
            )?;
            if self.config.max_distance_km > 0.0 && dist > self.config.max_distance_km {
                continue;
            }
            let hypocentral_km = (dist.powi(2) + earthquake.depth.max(0.0).powi(2)).sqrt();
            let estimated_intensity =
                intensity::estimate_intensity(earthquake.magnitude, hypocentral_km);
            let level = subscription.level_for_intensity(estimated_intensity)?;
            let timing = AlertTiming {
                distance_km: dist,
                hypocentral_km,
                estimated_intensity,
                seconds_to_p: seconds_until_arrival(
                    earthquake,
                    hypocentral_km,
                    self.config.p_wave_km_s,
                ),
                seconds_to_s: seconds_until_arrival(
                    earthquake,
                    hypocentral_km,
                    self.config.s_wave_km_s,
                ),
            };
            let mut selected = subscription.clone();
            selected.location_name = location.name;
            selected.latitude = location.latitude;
            selected.longitude = location.longitude;
            let replace = best
                .as_ref()
                .map(|(_, _, current)| timing.distance_km < current.distance_km)
                .unwrap_or(true);
            if replace {
                best = Some((selected, level, timing));
            }
        }
        best
    }
}

fn earthquake_key(earthquake: &CommonEarthquakeInfo) -> String {
    if !earthquake.event_id.trim().is_empty() {
        format!("{}:{}", earthquake.source_type, earthquake.event_id)
    } else {
        format!(
            "{}:{:.3}:{:.3}:{:.1}:{}",
            earthquake.source_type,
            earthquake.latitude,
            earthquake.longitude,
            earthquake.magnitude,
            earthquake.origin_time
        )
    }
}

fn seconds_until_arrival(
    earthquake: &CommonEarthquakeInfo,
    hypocentral_km: f64,
    speed: f64,
) -> i64 {
    if speed <= 0.0 {
        return 0;
    }
    let travel_seconds = (hypocentral_km / speed).round() as i64;
    if let Some(origin_epoch) = parse_origin_epoch_seconds(earthquake) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        origin_epoch + travel_seconds - now
    } else {
        travel_seconds
    }
}

fn origin_age_seconds(earthquake: &CommonEarthquakeInfo) -> Option<i64> {
    let origin_epoch = parse_origin_epoch_seconds(earthquake)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(now - origin_epoch)
}

fn parse_origin_epoch_seconds(earthquake: &CommonEarthquakeInfo) -> Option<i64> {
    let offset = if earthquake.source_type == "jma_eew" {
        9 * 3600
    } else {
        8 * 3600
    };
    parse_datetime_epoch_seconds(&earthquake.origin_time, offset)
}

fn parse_datetime_epoch_seconds(value: &str, offset_seconds: i64) -> Option<i64> {
    let normalized = value.trim().replace('T', " ").replace('/', "-");
    let (date, time) = normalized.split_once(' ')?;
    let mut date_parts = date.split('-').filter_map(|part| part.parse::<i64>().ok());
    let year = date_parts.next()?;
    let month = date_parts.next()?;
    let day = date_parts.next()?;
    let mut time_parts = time.split(':').filter_map(|part| part.parse::<i64>().ok());
    let hour = time_parts.next()?;
    let minute = time_parts.next()?;
    let second = time_parts.next().unwrap_or(0);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_prime + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_space_slash_and_timestamps_with_timezone_offsets() {
        let beijing = parse_datetime_epoch_seconds("2026-07-07 09:30:00", 8 * 3600);
        let slash = parse_datetime_epoch_seconds("2026/07/07 09:30:00", 8 * 3600);
        let jst = parse_datetime_epoch_seconds("2026-07-07T10:30:00", 9 * 3600);

        assert_eq!(beijing, slash);
        assert_eq!(beijing, jst);
        assert!(beijing.is_some());
    }
}
