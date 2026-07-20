use crate::storage::Storage;
use anyhow::Result;
use std::path::Path;

pub use crate::models::{
    AlertRule, DestinationId, DisasterCategory, GeoPoint, MonitoringTarget,
    NotificationDestination, Subscription,
};

pub struct MigrationStorage {
    storage: Storage,
}

impl MigrationStorage {
    pub fn open(path: impl AsRef<Path>, data_encryption_key: [u8; 32]) -> Result<Self> {
        Ok(Self {
            storage: Storage::open_with_key(path, data_encryption_key)?,
        })
    }

    pub fn bind_source(&self, source_fingerprint: [u8; 32], existing_partial: bool) -> Result<()> {
        self.storage
            .bind_subscription_migration(source_fingerprint, existing_partial)
    }

    pub fn subscription(&self, destination: &DestinationId) -> Result<Option<Subscription>> {
        self.storage
            .subscription_manager()
            .get_subscription(destination)
    }

    pub fn import_subscriptions(&self, subscriptions: Vec<Subscription>) -> Result<usize> {
        self.storage
            .subscription_manager()
            .import_subscriptions(subscriptions)
    }

    pub fn flush(&self) -> Result<()> {
        self.storage.inner().persist()
    }

    pub fn verify_source(&self, source_fingerprint: [u8; 32]) -> Result<()> {
        self.storage
            .verify_subscription_migration(source_fingerprint)
    }

    pub fn verify_postings(&self) -> Result<()> {
        self.storage.verify_migration_postings()
    }

    pub fn verify_matches(&self, source: &[Subscription]) -> Result<()> {
        self.storage.verify_migration_matches(source)
    }

    pub fn subscriptions(&self) -> Result<Vec<Subscription>> {
        self.storage.migration_subscriptions()
    }
}
