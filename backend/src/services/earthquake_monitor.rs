use crate::db::Database;
use crate::models::{CommonEarthquakeInfo, EarthquakeData, WebSocketMessage};
use crate::services::BarkNotifier;
use crate::utils::{distance, geohash, intensity};
use anyhow::Result;
use futures::stream::{self, StreamExt};
use futures_util::StreamExt as FuturesStreamExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const EEW_WEBSOCKET_URL: &str = "wss://ws-api.wolfx.jp/all_eew";
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// 地震监控服务（支持百万级并发）
pub struct EarthquakeMonitor {
    db: Database,
    bark_notifier: BarkNotifier,
    max_concurrent: usize,
    semaphore: Arc<Semaphore>,
}

impl EarthquakeMonitor {
    pub fn new(
        db: Database,
        bark_api_url: String,
        http_pool_size: usize,
        max_concurrent: usize,
        _batch_size: usize,
    ) -> Self {
        let subscription_store = db.subscriptions();
        let bark_notifier = BarkNotifier::new(bark_api_url, http_pool_size, subscription_store);
        let semaphore = Arc::new(Semaphore::new(max_concurrent));

        tracing::info!(
            "初始化地震监控服务: 最大并发={}, HTTP连接池={}",
            max_concurrent,
            http_pool_size
        );

        Self {
            db,
            bark_notifier,
            max_concurrent,
            semaphore,
        }
    }

    /// 启动监控（会自动重连）
    pub async fn start(&self) -> Result<()> {
        loop {
            tracing::info!("正在连接到地震预警 WebSocket...");

            match self.connect_and_monitor().await {
                Ok(_) => {
                    tracing::warn!("WebSocket 连接正常关闭");
                }
                Err(e) => {
                    tracing::error!("WebSocket 连接错误: {:?}", e);
                }
            }

            tracing::info!("{}秒后重新连接...", RECONNECT_DELAY.as_secs());
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    }

    /// 连接并监控 WebSocket
    async fn connect_and_monitor(&self) -> Result<()> {
        let (ws_stream, _) = connect_async(EEW_WEBSOCKET_URL).await?;
        tracing::info!("WebSocket 已连接到: {}", EEW_WEBSOCKET_URL);

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
            "接收到地震预警 [{}]: M{:.1} 深度{}km @ ({}, {}) - {}",
            common_info.source_type,
            common_info.magnitude,
            common_info.depth,
            common_info.latitude,
            common_info.longitude,
            common_info.region
        );

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

        tracing::info!(
            "震央 GeoHash: {}, 检查 {} 个格子",
            center_geohash,
            neighbor_geohashes.len()
        );

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
            // 计算精确距离
            let dist = distance::vincenty_distance(
                earthquake.latitude,
                earthquake.longitude,
                subscription.latitude,
                subscription.longitude,
            )
            .unwrap_or(0.0);

            // 估算用户所在位置的震度
            let estimated_intensity = intensity::estimate_intensity(earthquake.magnitude, dist);

            // 只有当预估震度 >= 用户设定的最小震度时才加入推送队列
            if estimated_intensity >= subscription.min_intensity {
                notification_tasks.push((subscription, dist, estimated_intensity));
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
            .map(|(subscription, dist, estimated_intensity)| {
                let bark_notifier = bark_notifier.clone();
                let semaphore = semaphore.clone();
                let earthquake = earthquake.clone();

                async move {
                    // 获取信号量许可（限流）
                    let _permit = semaphore.acquire().await.unwrap();

                    let bark_id = subscription.bark_id.clone();

                    tracing::debug!(
                        "推送给 {}: 距离 {:.1}km, 预估震度 {} >= 阈值 {}",
                        bark_id,
                        dist,
                        estimated_intensity,
                        subscription.min_intensity
                    );

                    match bark_notifier
                        .send_earthquake_alert(
                            &subscription,
                            &earthquake,
                            dist,
                            estimated_intensity,
                        )
                        .await
                    {
                        Ok(_) => (bark_id, true, None),
                        Err(e) => {
                            tracing::error!("推送失败 ({}): {:?}", bark_id, e);
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
}
