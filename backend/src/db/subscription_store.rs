use crate::models::{GeoHashIndex, Subscription};
use crate::utils::geohash;
use anyhow::{Result, anyhow};
use serde_json;
use sled::Db;

/// 订阅数据存储
#[derive(Clone)]
pub struct SubscriptionStore {
    db: Db,
}

impl SubscriptionStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 创建或更新订阅
    pub fn upsert_subscription(&self, subscription: Subscription) -> Result<()> {
        let bark_id = subscription.bark_id.clone();
        let geohash_str = geohash::encode(subscription.latitude, subscription.longitude);

        // 1. 检查是否已存在订阅
        let old_subscription = self.get_subscription(&bark_id).ok();
        let is_new_subscription = old_subscription.is_none();

        // 2. 如果存在旧订阅且位置变化，需要更新 GeoHash 索引
        if let Some(old_sub) = old_subscription {
            let old_geohash = geohash::encode(old_sub.latitude, old_sub.longitude);
            if old_geohash != geohash_str {
                // 从旧的 GeoHash 索引中移除
                self.remove_from_geohash_index(&bark_id, &old_geohash)?;
            }
        }

        // 3. 保存订阅数据
        let key = format!("sub:{}", bark_id);
        let value = serde_json::to_vec(&subscription)?;
        self.db.insert(key.as_bytes(), value)?;

        // 4. 添加到 GeoHash 索引
        self.add_to_geohash_index(&bark_id, &geohash_str)?;

        // 5. 只在新增订阅时更新统计计数
        if is_new_subscription {
            self.increment_subscription_count()?;
            tracing::info!("新订阅成功: bark_id={}, geohash={}", bark_id, geohash_str);
        } else {
            tracing::info!("更新订阅成功: bark_id={}, geohash={}", bark_id, geohash_str);
        }

        Ok(())
    }

    /// 删除订阅
    pub fn delete_subscription(&self, bark_id: &str) -> Result<()> {
        // 1. 获取订阅信息以获得 GeoHash
        let subscription = self.get_subscription(bark_id)?;
        let geohash_str = geohash::encode(subscription.latitude, subscription.longitude);

        // 2. 从 GeoHash 索引中移除
        self.remove_from_geohash_index(bark_id, &geohash_str)?;

        // 3. 删除订阅数据
        let key = format!("sub:{}", bark_id);
        self.db.remove(key.as_bytes())?;

        // 4. 更新统计计数
        self.decrement_subscription_count()?;

        tracing::info!("取消订阅成功: bark_id={}", bark_id);
        Ok(())
    }

    /// 获取订阅
    pub fn get_subscription(&self, bark_id: &str) -> Result<Subscription> {
        let key = format!("sub:{}", bark_id);
        let value = self
            .db
            .get(key.as_bytes())?
            .ok_or_else(|| anyhow!("订阅不存在"))?;

        let subscription: Subscription = serde_json::from_slice(&value)?;
        Ok(subscription)
    }

    /// 根据 GeoHash 获取订阅列表
    pub fn get_subscriptions_by_geohashes(
        &self,
        geohashes: &[String],
    ) -> Result<Vec<Subscription>> {
        let mut all_bark_ids = Vec::new();

        // 1. 收集所有相关 GeoHash 的 bark_ids
        for gh in geohashes {
            if let Ok(index) = self.get_geohash_index(gh) {
                all_bark_ids.extend(index.bark_ids);
            }
        }

        // 去重
        all_bark_ids.sort();
        all_bark_ids.dedup();

        // 2. 批量获取订阅详情
        let mut subscriptions = Vec::new();
        for bark_id in all_bark_ids {
            if let Ok(sub) = self.get_subscription(&bark_id) {
                subscriptions.push(sub);
            }
        }

        Ok(subscriptions)
    }

    /// 获取订阅总数
    pub fn get_total_count(&self) -> Result<usize> {
        let key = b"stats:total";
        if let Some(value) = self.db.get(key)? {
            let count_bytes: [u8; 8] = value
                .as_ref()
                .try_into()
                .map_err(|_| anyhow!("统计数据格式错误"))?;
            Ok(u64::from_be_bytes(count_bytes) as usize)
        } else {
            Ok(0)
        }
    }

    /// 添加到 GeoHash 索引
    fn add_to_geohash_index(&self, bark_id: &str, geohash: &str) -> Result<()> {
        let key = format!("geo:{}", geohash);

        let mut index = self.get_geohash_index(geohash).unwrap_or_default();
        index.add(bark_id.to_string());

        let value = serde_json::to_vec(&index)?;
        self.db.insert(key.as_bytes(), value)?;

        Ok(())
    }

    /// 从 GeoHash 索引中移除
    fn remove_from_geohash_index(&self, bark_id: &str, geohash: &str) -> Result<()> {
        let key = format!("geo:{}", geohash);

        if let Ok(mut index) = self.get_geohash_index(geohash) {
            index.remove(bark_id);

            if index.bark_ids.is_empty() {
                // 如果索引为空，删除该键
                self.db.remove(key.as_bytes())?;
            } else {
                let value = serde_json::to_vec(&index)?;
                self.db.insert(key.as_bytes(), value)?;
            }
        }

        Ok(())
    }

    /// 获取 GeoHash 索引
    fn get_geohash_index(&self, geohash: &str) -> Result<GeoHashIndex> {
        let key = format!("geo:{}", geohash);
        let value = self
            .db
            .get(key.as_bytes())?
            .ok_or_else(|| anyhow!("GeoHash 索引不存在"))?;

        let index: GeoHashIndex = serde_json::from_slice(&value)?;
        Ok(index)
    }

    /// 增加订阅计数
    fn increment_subscription_count(&self) -> Result<()> {
        let key = b"stats:total";
        let count = self.get_total_count()? + 1;
        let value = (count as u64).to_be_bytes();
        self.db.insert(key, &value[..])?;
        Ok(())
    }

    /// 减少订阅计数
    fn decrement_subscription_count(&self) -> Result<()> {
        let key = b"stats:total";
        let count = self.get_total_count()?.saturating_sub(1);
        let value = (count as u64).to_be_bytes();
        self.db.insert(key, &value[..])?;
        Ok(())
    }
}
