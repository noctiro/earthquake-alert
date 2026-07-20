use crate::models::{DestinationId, Subscription};
use crate::storage::{FjallStorage, decode_record, encode_record};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Debug)]
pub(crate) enum DeleteSubscriptionError {
    NotFound,
    Storage(anyhow::Error),
}

impl fmt::Display for DeleteSubscriptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("订阅不存在"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DeleteSubscriptionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFound => None,
            Self::Storage(error) => error.source(),
        }
    }
}

impl From<anyhow::Error> for DeleteSubscriptionError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone)]
pub(crate) struct SubscriptionManager {
    storage: FjallStorage,
}

impl SubscriptionManager {
    pub(crate) fn new(storage: FjallStorage) -> Self {
        Self { storage }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmationOperation {
    id: u64,
    subscription: Subscription,
    state: ConfirmationState,
    due_at_ms: i64,
    lease_until_ms: Option<i64>,
    lease_token: Option<u64>,
    lease_generation: u64,
    attempts: u16,
    created_at_ms: i64,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ConfirmationState {
    Ready,
    Leased,
}

#[derive(Debug, Clone)]
pub(crate) struct LeasedSubscriptionConfirmation {
    pub(crate) id: u64,
    pub(crate) subscription: Subscription,
    pub(crate) lease_token: u64,
    pub(crate) attempts: u16,
    pub(crate) created_at_ms: i64,
}

impl SubscriptionManager {
    #[cfg(any(test, feature = "benchmarks"))]
    pub(crate) fn upsert_subscription(&self, subscription: Subscription) -> Result<()> {
        self.storage.store_subscription(subscription)?;
        Ok(())
    }

    #[cfg(feature = "migration")]
    pub(crate) fn import_subscriptions(&self, subscriptions: Vec<Subscription>) -> Result<usize> {
        self.storage.import_subscription_batch(subscriptions)
    }

    #[cfg(any(test, feature = "migration"))]
    pub(crate) fn get_subscription(
        &self,
        destination: &DestinationId,
    ) -> Result<Option<Subscription>> {
        Ok(self
            .storage
            .stored_subscription_by_destination(destination)?
            .filter(|record| record.active)
            .map(|record| record.subscription))
    }

    pub(crate) fn delete_subscription(
        &self,
        destination: &DestinationId,
    ) -> std::result::Result<(), DeleteSubscriptionError> {
        let Some(record) = self
            .storage
            .stored_subscription_by_destination(destination)?
        else {
            return if self.remove_confirmation_for_destination(destination)? {
                Ok(())
            } else {
                Err(DeleteSubscriptionError::NotFound)
            };
        };
        if !self.storage.delete_subscription(record.id)? {
            return Err(DeleteSubscriptionError::NotFound);
        }
        Ok(())
    }

    pub(crate) fn total_count(&self) -> Result<usize> {
        self.storage.active_subscription_count()
    }

    pub(crate) fn begin_confirmation(
        &self,
        subscription: Subscription,
        now_ms: i64,
        lease_for_ms: i64,
    ) -> Result<LeasedSubscriptionConfirmation> {
        subscription
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid subscription: {error}"))?;
        let id = self.storage.next_id("confirmation")?;
        let lease_generation = 1;
        let token = confirmation_token(id, lease_generation);
        let operation = ConfirmationOperation {
            id,
            subscription,
            state: ConfirmationState::Leased,
            due_at_ms: now_ms,
            lease_until_ms: Some(now_ms.saturating_add(lease_for_ms.max(1_000))),
            lease_token: Some(token),
            lease_generation,
            attempts: 0,
            created_at_ms: now_ms,
            last_error: None,
        };
        self.storage.begin_confirmation(
            operation.id,
            &operation.subscription.destination_id(),
            encode_record(&operation)?,
        )?;
        Ok(leased(operation, token))
    }

    pub(crate) fn lease_due_confirmations(
        &self,
        now_ms: i64,
        lease_for_ms: i64,
        max_age_ms: i64,
        limit: usize,
    ) -> Result<Vec<LeasedSubscriptionConfirmation>> {
        let mut leased_values = Vec::new();
        for operation in self.confirmations()? {
            if now_ms.saturating_sub(operation.created_at_ms) >= max_age_ms {
                let expected = encode_record(&operation)?;
                self.storage.replace_confirmation(
                    operation.id,
                    &expected,
                    None,
                    Some(&operation.subscription.destination_id()),
                )?;
                continue;
            }
            let due = match operation.state {
                ConfirmationState::Ready => operation.due_at_ms <= now_ms,
                ConfirmationState::Leased => operation
                    .lease_until_ms
                    .is_some_and(|value| value <= now_ms),
            };
            if !due {
                continue;
            }
            if leased_values.len() >= limit {
                break;
            }
            let expected = encode_record(&operation)?;
            let mut leased_operation = operation;
            leased_operation.lease_generation = leased_operation
                .lease_generation
                .checked_add(1)
                .context("confirmation lease generation exhausted")?;
            let token = confirmation_token(leased_operation.id, leased_operation.lease_generation);
            leased_operation.state = ConfirmationState::Leased;
            leased_operation.lease_until_ms = Some(now_ms.saturating_add(lease_for_ms.max(1_000)));
            leased_operation.lease_token = Some(token);
            if self.storage.replace_confirmation(
                leased_operation.id,
                &expected,
                Some(encode_record(&leased_operation)?),
                None,
            )? {
                leased_values.push(leased(leased_operation, token));
            }
        }
        Ok(leased_values)
    }

    pub(crate) fn renew_confirmation_lease(
        &self,
        id: u64,
        token: u64,
        now_ms: i64,
        lease_for_ms: i64,
    ) -> Result<bool> {
        let Some(mut operation) = self.confirmation(id)? else {
            return Ok(false);
        };
        if operation.state != ConfirmationState::Leased || operation.lease_token != Some(token) {
            return Ok(false);
        }
        let expected = encode_record(&operation)?;
        operation.lease_until_ms = Some(now_ms.saturating_add(lease_for_ms.max(1_000)));
        self.storage
            .replace_confirmation(id, &expected, Some(encode_record(&operation)?), None)
    }

    pub(crate) fn activate_confirmation(&self, id: u64, token: u64) -> Result<bool> {
        let Some(operation) = self.confirmation(id)? else {
            return Ok(false);
        };
        if operation.lease_token != Some(token) {
            return Ok(false);
        }
        let encoded = encode_record(&operation)?;
        self.storage
            .activate_confirmation(id, &encoded, operation.subscription)
    }

    pub(crate) fn reschedule_confirmation(
        &self,
        id: u64,
        token: u64,
        attempted_at_ms: i64,
        due_at_ms: i64,
        error: &str,
    ) -> Result<bool> {
        let Some(mut operation) = self.confirmation(id)? else {
            return Ok(false);
        };
        if operation.lease_token != Some(token) {
            return Ok(false);
        }
        let expected = encode_record(&operation)?;
        operation.state = ConfirmationState::Ready;
        operation.due_at_ms = due_at_ms.max(attempted_at_ms);
        operation.lease_until_ms = None;
        operation.lease_token = None;
        operation.attempts = operation.attempts.saturating_add(1);
        operation.last_error = Some(error.chars().take(1_024).collect());
        self.storage
            .replace_confirmation(id, &expected, Some(encode_record(&operation)?), None)
    }

    pub(crate) fn abandon_confirmation(&self, id: u64, token: u64) -> Result<bool> {
        let Some(operation) = self.confirmation(id)? else {
            return Ok(false);
        };
        if operation.lease_token != Some(token) {
            return Ok(false);
        }
        self.storage.replace_confirmation(
            id,
            &encode_record(&operation)?,
            None,
            Some(&operation.subscription.destination_id()),
        )
    }

    pub(crate) fn pending_confirmation_count(&self) -> Result<usize> {
        Ok(self.confirmations()?.len())
    }

    fn confirmations(&self) -> Result<Vec<ConfirmationOperation>> {
        self.storage
            .confirmation_records()?
            .into_iter()
            .map(|bytes| decode_record(&bytes).context("invalid confirmation record"))
            .collect()
    }

    fn confirmation(&self, id: u64) -> Result<Option<ConfirmationOperation>> {
        self.storage
            .confirmation_record(id)?
            .map(|value| decode_record(&value).context("invalid confirmation record"))
            .transpose()
    }

    fn remove_confirmation_for_destination(&self, destination: &DestinationId) -> Result<bool> {
        self.storage
            .remove_confirmation_for_destination(destination)
    }
}

fn leased(operation: ConfirmationOperation, token: u64) -> LeasedSubscriptionConfirmation {
    LeasedSubscriptionConfirmation {
        id: operation.id,
        subscription: operation.subscription,
        lease_token: token,
        attempts: operation.attempts,
        created_at_ms: operation.created_at_ms,
    }
}

fn confirmation_token(id: u64, generation: u64) -> u64 {
    let mut hash = Sha256::new();
    hash.update(b"disaster-alert:confirmation-lease:v1\0");
    hash.update(id.to_be_bytes());
    hash.update(generation.to_be_bytes());
    let digest = hash.finalize();
    u64::from_be_bytes(digest[..8].try_into().unwrap_or([0; 8]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AlertRule, GeoPoint, MonitoringTarget, NotificationDestination};

    fn subscription() -> Subscription {
        Subscription::new(
            NotificationDestination::Bark {
                base_url: "https://api.day.app".to_string(),
                device_key: "device1".to_string(),
            },
            vec![MonitoringTarget {
                label: "home".to_string(),
                point: GeoPoint {
                    latitude: 35.0,
                    longitude: 105.0,
                },
                region: Default::default(),
            }],
            vec![AlertRule::default_for(
                crate::models::DisasterCategory::EarthquakeReport,
            )],
        )
    }

    fn subscription_with_label(label: &str) -> Subscription {
        let mut value = subscription();
        value.targets[0].label = label.to_string();
        value
    }

    #[test]
    fn confirmation_activation_compiles_subscription() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = FjallStorage::open(directory.path())?;
        let manager = SubscriptionManager::new(storage);
        let leased = manager.begin_confirmation(subscription(), 100, 1_000)?;
        anyhow::ensure!(manager.activate_confirmation(leased.id, leased.lease_token)?);
        anyhow::ensure!(manager.total_count()? == 1);
        Ok(())
    }

    #[test]
    fn newer_confirmation_supersedes_older_request() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = FjallStorage::open(directory.path())?;
        let manager = SubscriptionManager::new(storage);
        let older = manager.begin_confirmation(subscription_with_label("older"), 100, 1_000)?;
        let newer = manager.begin_confirmation(subscription_with_label("newer"), 101, 1_000)?;

        anyhow::ensure!(!manager.activate_confirmation(older.id, older.lease_token)?);
        anyhow::ensure!(manager.activate_confirmation(newer.id, newer.lease_token)?);
        let active = manager
            .get_subscription(&newer.subscription.destination_id())?
            .context("missing active subscription")?;
        anyhow::ensure!(active.targets[0].label == "newer");
        anyhow::ensure!(manager.pending_confirmation_count()? == 0);
        Ok(())
    }

    #[test]
    fn unsubscribe_cancels_pending_confirmation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = FjallStorage::open(directory.path())?;
        let manager = SubscriptionManager::new(storage);
        let leased = manager.begin_confirmation(subscription(), 100, 1_000)?;
        let destination = leased.subscription.destination_id();

        manager.delete_subscription(&destination)?;
        anyhow::ensure!(manager.pending_confirmation_count()? == 0);
        anyhow::ensure!(!manager.activate_confirmation(leased.id, leased.lease_token)?);
        anyhow::ensure!(manager.get_subscription(&destination)?.is_none());
        anyhow::ensure!(matches!(
            manager.delete_subscription(&destination),
            Err(DeleteSubscriptionError::NotFound)
        ));
        Ok(())
    }

    #[test]
    fn unsubscribe_invalidates_active_and_pending_generations() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = FjallStorage::open(directory.path())?;
        let manager = SubscriptionManager::new(storage);
        let active = manager.begin_confirmation(subscription_with_label("active"), 100, 1_000)?;
        anyhow::ensure!(manager.activate_confirmation(active.id, active.lease_token)?);
        let pending = manager.begin_confirmation(subscription_with_label("pending"), 101, 1_000)?;
        let destination = pending.subscription.destination_id();

        manager.delete_subscription(&destination)?;
        anyhow::ensure!(manager.get_subscription(&destination)?.is_none());
        anyhow::ensure!(!manager.activate_confirmation(pending.id, pending.lease_token)?);
        anyhow::ensure!(manager.pending_confirmation_count()? == 0);
        Ok(())
    }

    #[test]
    fn due_confirmation_after_many_future_records_is_not_starved() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = FjallStorage::open(directory.path())?;
        let manager = SubscriptionManager::new(storage);
        for index in 0..512 {
            let mut value = subscription();
            let NotificationDestination::Bark { device_key, .. } = &mut value.destination;
            *device_key = format!("future{index}");
            drop(manager.begin_confirmation(value, 10_000, 100_000)?);
        }
        let mut due = subscription();
        let NotificationDestination::Bark { device_key, .. } = &mut due.destination;
        *device_key = "due".to_string();
        let expected = manager.begin_confirmation(due, 0, 1_000)?;

        let leased = manager.lease_due_confirmations(2_000, 1_000, i64::MAX, 1)?;
        anyhow::ensure!(leased.len() == 1);
        anyhow::ensure!(leased[0].id == expected.id);
        Ok(())
    }

    #[test]
    fn expired_confirmation_is_removed_before_leasing() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = FjallStorage::open(directory.path())?;
        let manager = SubscriptionManager::new(storage);
        drop(manager.begin_confirmation(subscription(), 0, 1_000)?);

        anyhow::ensure!(
            manager
                .lease_due_confirmations(10_000, 1_000, 10_000, 1)?
                .is_empty()
        );
        anyhow::ensure!(manager.pending_confirmation_count()? == 0);
        Ok(())
    }
}
