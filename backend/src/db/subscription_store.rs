use crate::models::{GeoHashIndex, Subscription, mask_bark_id};
use crate::utils::geohash;
use anyhow::{Result, anyhow};
use sled::Db;
use std::collections::HashSet;

#[derive(Clone)]
pub struct SubscriptionStore {
    db: Db,
}

impl SubscriptionStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub fn upsert_subscription(&self, subscription: Subscription) -> Result<()> {
        let bark_id = subscription.bark_id.clone();
        let new_geohashes = subscription_geohashes(&subscription);

        let old_subscription = self.get_subscription(&bark_id).ok();
        let is_new_subscription = old_subscription.is_none();

        if let Some(old_sub) = old_subscription {
            for old_geohash in subscription_geohashes(&old_sub) {
                if !new_geohashes.contains(&old_geohash) {
                    self.remove_from_geohash_index(&bark_id, &old_geohash)?;
                }
            }
        }

        let key = format!("sub:{}", bark_id);
        let value = serde_json::to_vec(&subscription)?;
        self.db.insert(key.as_bytes(), value)?;

        for geohash_str in &new_geohashes {
            self.add_to_geohash_index(&bark_id, geohash_str)?;
        }

        if is_new_subscription {
            self.increment_subscription_count()?;
            tracing::info!(
                event = "subscription.stored",
                action = "insert",
                bark_id = %mask_bark_id(&bark_id),
                geohash_count = new_geohashes.len(),
                "subscription.stored"
            );
        } else {
            tracing::info!(
                event = "subscription.stored",
                action = "update",
                bark_id = %mask_bark_id(&bark_id),
                geohash_count = new_geohashes.len(),
                "subscription.stored"
            );
        }

        Ok(())
    }

    pub fn delete_subscription(&self, bark_id: &str) -> Result<()> {
        let subscription = self.get_subscription(bark_id)?;

        for geohash_str in subscription_geohashes(&subscription) {
            self.remove_from_geohash_index(bark_id, &geohash_str)?;
        }

        let key = format!("sub:{}", bark_id);
        self.db.remove(key.as_bytes())?;

        self.decrement_subscription_count()?;

        tracing::info!(
            event = "subscription.deleted",
            bark_id = %mask_bark_id(bark_id),
            "subscription.deleted"
        );
        Ok(())
    }

    pub fn get_subscription(&self, bark_id: &str) -> Result<Subscription> {
        let key = format!("sub:{}", bark_id);
        let value = self
            .db
            .get(key.as_bytes())?
            .ok_or_else(|| anyhow!("订阅不存在"))?;

        let subscription: Subscription = serde_json::from_slice(&value)?;
        Ok(subscription)
    }

    pub fn get_subscriptions_by_geohashes(
        &self,
        geohashes: &[String],
    ) -> Result<Vec<Subscription>> {
        let mut all_bark_ids = Vec::new();

        for gh in geohashes {
            if let Ok(index) = self.get_geohash_index(gh) {
                all_bark_ids.extend(index.bark_ids);
            }
        }

        all_bark_ids.sort();
        all_bark_ids.dedup();

        let mut subscriptions = Vec::new();
        for bark_id in all_bark_ids {
            if let Ok(sub) = self.get_subscription(&bark_id) {
                subscriptions.push(sub);
            }
        }

        Ok(subscriptions)
    }

    pub fn get_total_count(&self) -> Result<usize> {
        let key = b"stats:total";
        if let Some(value) = self.db.get(key)? {
            let count_bytes: [u8; 8] = value
                .as_ref()
                .try_into()
                .map_err(|error| anyhow!("统计数据格式错误: {error:?}"))?;
            Ok(u64::from_be_bytes(count_bytes) as usize)
        } else {
            Ok(0)
        }
    }

    fn add_to_geohash_index(&self, bark_id: &str, geohash: &str) -> Result<()> {
        let key = format!("geo:{}", geohash);

        let mut index = self.get_geohash_index(geohash).unwrap_or_default();
        index.add(bark_id.to_string());

        let value = serde_json::to_vec(&index)?;
        self.db.insert(key.as_bytes(), value)?;

        Ok(())
    }

    fn remove_from_geohash_index(&self, bark_id: &str, geohash: &str) -> Result<()> {
        let key = format!("geo:{}", geohash);

        if let Ok(mut index) = self.get_geohash_index(geohash) {
            index.remove(bark_id);

            if index.bark_ids.is_empty() {
                self.db.remove(key.as_bytes())?;
            } else {
                let value = serde_json::to_vec(&index)?;
                self.db.insert(key.as_bytes(), value)?;
            }
        }

        Ok(())
    }

    fn get_geohash_index(&self, geohash: &str) -> Result<GeoHashIndex> {
        let key = format!("geo:{}", geohash);
        let value = self
            .db
            .get(key.as_bytes())?
            .ok_or_else(|| anyhow!("GeoHash 索引不存在"))?;

        let index: GeoHashIndex = serde_json::from_slice(&value)?;
        Ok(index)
    }

    fn increment_subscription_count(&self) -> Result<()> {
        let key = b"stats:total";
        let count = self.get_total_count()? + 1;
        let value = (count as u64).to_be_bytes();
        self.db.insert(key, &value[..])?;
        Ok(())
    }

    fn decrement_subscription_count(&self) -> Result<()> {
        let key = b"stats:total";
        let count = self.get_total_count()?.saturating_sub(1);
        let value = (count as u64).to_be_bytes();
        self.db.insert(key, &value[..])?;
        Ok(())
    }
}

fn subscription_geohashes(subscription: &Subscription) -> HashSet<String> {
    subscription
        .normalized_locations()
        .into_iter()
        .map(|location| geohash::encode(location.latitude, location.longitude))
        .collect()
}
