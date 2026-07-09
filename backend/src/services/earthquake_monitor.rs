use crate::config::Config;
use crate::db::Database;
use crate::models::{
    CommonEarthquakeInfo, EarthquakeData, Subscription, WebSocketMessage, mask_bark_id,
};
use crate::services::{AlertTiming, BarkNotifier};
use crate::utils::{distance, geohash, intensity};
use anyhow::Result;
use futures_util::{StreamExt, stream};
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

/// 监听 EEW WebSocket，并把匹配订阅的事件转成 Bark 推送
pub struct EarthquakeMonitor {
    db: Database,
    bark_notifier: BarkNotifier,
    max_concurrent: usize,
    semaphore: Arc<Semaphore>,
    config: MonitorConfig,
    seen_events: Arc<Mutex<HashMap<String, SeenEvent>>>,
}

impl EarthquakeMonitor {
    pub fn new(db: Database, config: Config, bark_notifier: BarkNotifier) -> Result<Self> {
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
            event = "monitor.initialized",
            max_concurrent,
            http_pool_size = config.http_pool_size,
            websocket_url = %monitor_config.websocket_url,
            "monitor.initialized"
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

    /// 启动 WebSocket 循环；连接断开后按指数退避重连
    pub async fn start(&self) -> Result<()> {
        let mut reconnect_delay = self.config.reconnect_min;
        loop {
            tracing::info!(
                event = "websocket.connecting",
                websocket_url = %self.config.websocket_url,
                "websocket.connecting"
            );

            match self.connect_and_monitor().await {
                Ok(_) => {
                    tracing::warn!(event = "websocket.closed", "websocket.closed");
                    reconnect_delay = self.config.reconnect_min;
                }
                Err(e) => {
                    tracing::error!(event = "websocket.error", error = ?e, "websocket.error");
                }
            }

            tracing::info!(
                event = "websocket.reconnect_scheduled",
                delay_seconds = reconnect_delay.as_secs(),
                "websocket.reconnect_scheduled"
            );
            tokio::time::sleep(reconnect_delay).await;
            reconnect_delay = (reconnect_delay * 2).min(self.config.reconnect_max);
        }
    }

    async fn connect_and_monitor(&self) -> Result<()> {
        let (ws_stream, _) = connect_async(&self.config.websocket_url).await?;
        tracing::info!(
            event = "websocket.connected",
            websocket_url = %self.config.websocket_url,
            "websocket.connected"
        );

        let (mut _write, mut read) = ws_stream.split();

        while let Some(message) = read.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    if let Err(e) = self.handle_earthquake_message(&text).await {
                        tracing::error!(event = "eew.handle_failed", error = ?e, "eew.handle_failed");
                    }
                }
                Ok(Message::Close(_)) => {
                    tracing::info!(event = "websocket.close_frame", "websocket.close_frame");
                    break;
                }
                Ok(Message::Ping(_)) => {
                    // tokio-tungstenite 会自动处理 pong
                    tracing::debug!(event = "websocket.ping", "websocket.ping");
                }
                Err(e) => {
                    tracing::error!(event = "websocket.message_error", error = ?e, "websocket.message_error");
                    return Err(e.into());
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_earthquake_message(&self, message: &str) -> Result<()> {
        let msg_wrapper: WebSocketMessage = match serde_json::from_str(message) {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!(
                    event = "eew.message_type_parse_failed",
                    error = ?e,
                    message_len = message.len(),
                    "eew.message_type_parse_failed"
                );
                return Ok(());
            }
        };

        match msg_wrapper.message_type.as_str() {
            "heartbeat" => {
                tracing::debug!(event = "websocket.heartbeat", "websocket.heartbeat");
                return Ok(());
            }
            "pong" => {
                tracing::debug!(event = "websocket.pong", "websocket.pong");
                return Ok(());
            }
            "jma_eqlist" | "cenc_eqlist" => {
                tracing::debug!(
                    event = "eew.list_ignored",
                    message_type = %msg_wrapper.message_type,
                    "eew.list_ignored"
                );
                return Ok(());
            }
            _ => {}
        }

        let common_info = match EarthquakeData::parse_to_common_info(message) {
            Ok(info) => info,
            Err(e) => {
                tracing::error!(
                    event = "eew.parse_failed",
                    message_type = %msg_wrapper.message_type,
                    error = ?e,
                    "eew.parse_failed"
                );
                return Ok(());
            }
        };

        tracing::info!(
            event = "eew.received",
            source_type = %common_info.source_type,
            event_id = %common_info.event_id,
            report_num = common_info.report_num,
            final_report = common_info.final_report,
            cancel = common_info.cancel,
            training = common_info.training,
            magnitude = common_info.magnitude,
            depth_km = common_info.depth,
            latitude = common_info.latitude,
            longitude = common_info.longitude,
            region = %common_info.region,
            "eew.received"
        );

        if self.should_skip_event(&common_info) {
            return Ok(());
        }

        self.notify_subscribers(&common_info).await?;

        Ok(())
    }

    async fn notify_subscribers(&self, earthquake: &CommonEarthquakeInfo) -> Result<()> {
        let start_time = Instant::now();

        let center_geohash = geohash::encode(earthquake.latitude, earthquake.longitude);
        let neighbor_geohashes = geohash::get_neighbors(&center_geohash);

        let event_key = earthquake_key(earthquake);
        tracing::info!(
            event = "notify.lookup_started",
            event_key = %event_key,
            center_geohash = %center_geohash,
            geohash_count = neighbor_geohashes.len(),
            "notify.lookup_started"
        );

        let store = self.db.subscriptions();
        let subscriptions = store.get_subscriptions_by_geohashes(&neighbor_geohashes)?;

        let total_candidates = subscriptions.len();
        tracing::info!(
            event = "notify.candidates_loaded",
            event_key = %event_key,
            candidate_count = total_candidates,
            "notify.candidates_loaded"
        );

        if total_candidates == 0 {
            tracing::info!(
                event = "notify.skipped",
                event_key = %event_key,
                reason = "no_candidates",
                "notify.skipped"
            );
            return Ok(());
        }

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
            event = "notify.filtered",
            event_key = %event_key,
            notification_count = tasks_count,
            filtered_count = total_candidates - tasks_count,
            "notify.filtered"
        );

        if tasks_count == 0 {
            tracing::info!(
                event = "notify.skipped",
                event_key = %event_key,
                reason = "below_threshold",
                "notify.skipped"
            );
            return Ok(());
        }

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
                    let permit = semaphore.acquire_owned().await;
                    let permit_guard: OwnedSemaphorePermit = match permit {
                        Ok(permit) => permit,
                        Err(error) => {
                            tracing::error!(
                                event = "notify.permit_failed",
                                error = ?error,
                                "notify.permit_failed"
                            );
                            return (bark_id, false, None);
                        }
                    };
                    let _permit_guard = permit_guard;

                    tracing::debug!(
                        event = "notify.send_started",
                        bark_id = %mask_bark_id(&bark_id),
                        distance_km = timing.distance_km,
                        estimated_intensity = timing.estimated_intensity,
                        level = %level,
                        "notify.send_started"
                    );

                    match bark_notifier
                        .send_earthquake_alert(&subscription, &level, &earthquake, &timing)
                        .await
                    {
                        Ok(_) => (bark_id, true, None),
                        Err(e) => {
                            tracing::error!(
                                event = "notify.send_failed",
                                bark_id = %mask_bark_id(&bark_id),
                                error = ?e,
                                "notify.send_failed"
                            );
                            (bark_id, false, Some(e))
                        }
                    }
                }
            })
            .buffer_unordered(self.max_concurrent)
            .collect::<Vec<_>>()
            .await;

        let notified_count = results.iter().filter(|(_, success, _)| *success).count();
        let error_count = results.iter().filter(|(_, success, _)| !*success).count();

        let elapsed = start_time.elapsed();

        tracing::info!(
            event = "notify.completed",
            event_key = %event_key,
            candidate_count = total_candidates,
            notified_count,
            error_count,
            elapsed_seconds = elapsed.as_secs_f64(),
            throughput_per_second = if elapsed.as_secs_f64() > 0.0 {
                notified_count as f64 / elapsed.as_secs_f64()
            } else {
                0.0
            },
            "notify.completed"
        );

        Ok(())
    }

    fn should_skip_event(&self, earthquake: &CommonEarthquakeInfo) -> bool {
        if earthquake.training && self.config.ignore_training {
            tracing::info!(
                event = "eew.skipped",
                reason = "training",
                event_key = %earthquake_key(earthquake),
                "eew.skipped"
            );
            return true;
        }
        if earthquake.cancel && self.config.ignore_cancel {
            tracing::info!(
                event = "eew.skipped",
                reason = "cancel",
                event_key = %earthquake_key(earthquake),
                "eew.skipped"
            );
            return true;
        }
        if self.config.stale_origin_seconds > 0
            && let Some(age_seconds) = origin_age_seconds(earthquake)
            && age_seconds > self.config.stale_origin_seconds
        {
            tracing::info!(
                event = "eew.skipped",
                reason = "stale_origin",
                event_key = %earthquake_key(earthquake),
                age_seconds,
                stale_origin_seconds = self.config.stale_origin_seconds,
                "eew.skipped"
            );
            return true;
        }

        let mut seen = match self.seen_events.lock() {
            Ok(seen) => seen,
            Err(error) => {
                tracing::error!(event = "dedup.lock_poisoned", error = ?error, "dedup.lock_poisoned");
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
                    event = "eew.skipped",
                    reason = "duplicate",
                    event_key = %key,
                    previous_report_num = previous.report_num,
                    report_num = earthquake.report_num,
                    "eew.skipped"
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
