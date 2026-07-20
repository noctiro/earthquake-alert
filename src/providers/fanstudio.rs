use super::fanstudio_protocol::{parse_fanstudio_snapshot, parse_fanstudio_update_value};
use super::reconnect;
use crate::config::Config;
use crate::models::ProviderChannel;
use crate::providers::ProviderCursor;
use crate::runtime::EventRuntime;
use crate::runtime::RuntimeStatus;
use crate::source_registry::SOURCES;
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct FanStudioSource {
    event_runtime: EventRuntime,
    websocket_url: String,
    reconnect_min: Duration,
    reconnect_max: Duration,
    runtime_status: RuntimeStatus,
}

impl FanStudioSource {
    pub(crate) fn new(
        config: &Config,
        event_runtime: EventRuntime,
        runtime_status: RuntimeStatus,
    ) -> Self {
        Self {
            event_runtime,
            websocket_url: config.fanstudio_websocket_url.clone(),
            reconnect_min: Duration::from_secs(config.reconnect_min_seconds),
            reconnect_max: Duration::from_secs(config.reconnect_max_seconds),
            runtime_status,
        }
    }

    pub(crate) async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let mut delay = self.reconnect_min;
        loop {
            if *shutdown.borrow() {
                break;
            }
            match self.connect_once(&mut delay, &mut shutdown).await {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => tracing::error!(
                    event = "fanstudio.websocket_error",
                    error = ?error,
                    "fanstudio.websocket_error"
                ),
            }
            self.runtime_status.fanstudio().set_connected(false);
            self.runtime_status.fanstudio().record_reconnect();
            tokio::select! {
                biased;
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                () = tokio::time::sleep(delay) => {}
            }
            delay = delay.saturating_mul(2).min(self.reconnect_max);
        }
        self.runtime_status.fanstudio().set_connected(false);
        Ok(())
    }

    async fn connect_once(
        &self,
        delay: &mut Duration,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<bool> {
        let connect = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async_with_config(
                &self.websocket_url,
                Some(
                    WebSocketConfig::default()
                        .max_message_size(Some(MAX_WEBSOCKET_MESSAGE_BYTES))
                        .max_frame_size(Some(MAX_WEBSOCKET_MESSAGE_BYTES)),
                ),
                false,
            ),
        );
        let (socket, _) = tokio::select! {
            biased;
            result = shutdown.changed() => {
                return Ok(result.is_err() || *shutdown.borrow());
            }
            result = connect => result
                .map_err(|error| anyhow::anyhow!("Fan Studio connection timed out: {error}"))??,
        };
        self.runtime_status.fanstudio().set_connected(true);
        let connected_at = Instant::now();
        tracing::info!(
            event = "fanstudio.connected",
            websocket_url = %self.websocket_url,
            "fanstudio.connected"
        );
        let (mut write, mut read) = socket.split();
        let outcome: Result<bool> = async {
            let mut streams = SOURCES
            .iter()
            .filter(|source| source.channel == ProviderChannel::FanStudio)
            .map(|source| source.provider_key.to_string())
            .collect::<Vec<_>>();
        streams.sort_unstable();
        streams.dedup();
        let mut source_md5 = self
            .event_runtime
            .provider_cursors(ProviderChannel::FanStudio, streams)
            .await?
            .into_iter()
            .map(|cursor| (cursor.stream, cursor.value))
            .collect::<HashMap<_, _>>();
        loop {
            if *shutdown.borrow() {
                return Ok(true);
            }
            let message = tokio::select! {
                biased;
                result = tokio::time::timeout(Duration::from_secs(90), read.next()) => result
                    .map_err(|error| anyhow::anyhow!("Fan Studio heartbeat timed out: {error}"))?,
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        return Ok(true);
                    }
                    continue;
                }
            };
            let Some(message) = message else { break };
            match message? {
                Message::Text(text) => {
                    self.runtime_status.fanstudio().record_message();
                    let envelope: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(value) => value,
                        Err(error) => {
                            self.runtime_status.fanstudio().record_parse_error();
                            tracing::warn!(
                                event = "fanstudio.invalid_json",
                                error = ?error,
                                "fanstudio.invalid_json"
                            );
                            continue;
                        }
                    };
                    match envelope.get("type").and_then(serde_json::Value::as_str) {
                        Some("heartbeat") => {
                            let send = tokio::time::timeout(
                                Duration::from_secs(10),
                                write.send(Message::Text("ping".into())),
                            );
                            tokio::select! {
                                biased;
                                result = shutdown.changed() => {
                                    if result.is_err() || *shutdown.borrow() {
                                        return Ok(true);
                                    }
                                }
                                result = send => {
                                    result.map_err(|error| anyhow::anyhow!("Fan Studio heartbeat response timed out: {error}"))??;
                                }
                            }
                        }
                        Some("initial_all" | "query_response") => {
                            self.submit_snapshot(&envelope, &mut source_md5).await?
                        }
                        Some("update") if is_new_update(&envelope, &source_md5) => {
                            let source = envelope
                                .get("source")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default();
                            if crate::source_registry::find_provider(
                                ProviderChannel::FanStudio,
                                source,
                            )
                            .is_none()
                            {
                                self.runtime_status.fanstudio().record_parse_error();
                                tracing::warn!(
                                    event = "fanstudio.unsupported_source",
                                    source,
                                    "fanstudio.unsupported_source"
                                );
                                continue;
                            }
                            match parse_fanstudio_update_value(&envelope) {
                                Ok(events) => {
                                    let cursor = match update_cursor(&envelope) {
                                        Ok(cursor) => cursor,
                                        Err(error) => {
                                            self.runtime_status.fanstudio().record_parse_error();
                                            tracing::warn!(
                                                event = "fanstudio.invalid_cursor",
                                                error = ?error,
                                                "fanstudio.invalid_cursor"
                                            );
                                            continue;
                                        }
                                    };
                                    let accepted = self
                                        .event_runtime
                                        .submit_provider_batch(
                                            ProviderChannel::FanStudio,
                                            events,
                                            cursor,
                                        )
                                        .await;
                                    if !accepted {
                                        anyhow::bail!(
                                            "Fan Studio update was not durably committed"
                                        );
                                    }
                                    commit_update_revision(&envelope, &mut source_md5);
                                }
                                Err(error) => {
                                    self.runtime_status.fanstudio().record_parse_error();
                                    tracing::warn!(
                                        event = "fanstudio.update_parse_failed",
                                        error = ?error,
                                        "fanstudio.update_parse_failed"
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
            reconnect::reset_after_healthy_uptime(
                delay,
                self.reconnect_min,
                connected_at.elapsed(),
            );
        }
            Ok(false)
        }
        .await;
        reconnect::reset_after_healthy_uptime(delay, self.reconnect_min, connected_at.elapsed());
        outcome
    }

    async fn submit_snapshot(
        &self,
        envelope: &serde_json::Value,
        source_md5: &mut HashMap<String, String>,
    ) -> Result<()> {
        for (source, parsed) in parse_fanstudio_snapshot(envelope) {
            match parsed {
                Ok(batch) => {
                    if !snapshot_is_new(&batch.source, batch.md5.as_deref(), source_md5) {
                        continue;
                    }
                    let cursor = match batch.md5.as_deref() {
                        Some(md5) => match ProviderCursor::new(&batch.source, md5) {
                            Ok(cursor) => Some(cursor),
                            Err(error) => {
                                self.runtime_status.fanstudio().record_parse_error();
                                tracing::warn!(event = "fanstudio.invalid_cursor", source, error = ?error, "fanstudio.invalid_cursor");
                                continue;
                            }
                        },
                        None => {
                            // Preserve the disaster data even when the provider cannot supply a
                            // replay cursor. The durable cursor remains unchanged.
                            self.runtime_status.fanstudio().record_parse_error();
                            tracing::warn!(
                                event = "fanstudio.missing_snapshot_cursor",
                                source,
                                "fanstudio.missing_snapshot_cursor"
                            );
                            None
                        }
                    };
                    let accepted = self
                        .event_runtime
                        .submit_provider_snapshot_batch(
                            ProviderChannel::FanStudio,
                            batch.events,
                            cursor,
                        )
                        .await;
                    if !accepted {
                        anyhow::bail!("Fan Studio snapshot was not durably committed for {source}");
                    }
                    if let Some(md5) = batch.md5 {
                        source_md5.insert(batch.source, md5);
                    }
                }
                Err(error) => {
                    self.runtime_status.fanstudio().record_parse_error();
                    tracing::warn!(event = "fanstudio.snapshot_parse_failed", source, error = ?error, "fanstudio.snapshot_parse_failed");
                }
            }
        }
        Ok(())
    }
}

fn snapshot_is_new(source: &str, md5: Option<&str>, source_md5: &HashMap<String, String>) -> bool {
    !md5.is_some_and(|md5| source_md5.get(source).is_some_and(|current| current == md5))
}

fn update_cursor(envelope: &serde_json::Value) -> Result<ProviderCursor> {
    let source = envelope
        .get("source")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Fan Studio update has no source"))?;
    if crate::source_registry::find_provider(ProviderChannel::FanStudio, source).is_none() {
        anyhow::bail!("Fan Studio update has unsupported source {source}");
    }
    let md5 = envelope
        .get("md5")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Fan Studio update has no md5"))?;
    ProviderCursor::new(source, md5)
}

fn is_new_update(envelope: &serde_json::Value, source_md5: &HashMap<String, String>) -> bool {
    let source = envelope
        .get("source")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let md5 = envelope
        .get("md5")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !md5.is_empty() && source_md5.get(source).is_some_and(|current| current == md5) {
        return false;
    }
    true
}

fn commit_update_revision(envelope: &serde_json::Value, source_md5: &mut HashMap<String, String>) {
    if let (Some(source), Some(md5)) = (
        envelope.get("source").and_then(serde_json::Value::as_str),
        envelope.get("md5").and_then(serde_json::Value::as_str),
    ) && !source.is_empty()
        && !md5.is_empty()
    {
        source_md5.insert(source.to_string(), md5.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppresses_only_identical_source_revisions() {
        let mut revisions = HashMap::new();
        let first = serde_json::json!({"source":"cenc","md5":"a"});
        let duplicate = serde_json::json!({"source":"cenc","md5":"a"});
        let next = serde_json::json!({"source":"cenc","md5":"b"});
        assert!(is_new_update(&first, &revisions));
        commit_update_revision(&first, &mut revisions);
        assert!(!is_new_update(&duplicate, &revisions));
        assert!(is_new_update(&next, &revisions));
    }

    #[test]
    fn suppresses_identical_snapshots_but_accepts_cursorless_data() {
        let revisions = HashMap::from([("cenc".to_string(), "a".to_string())]);
        assert!(!snapshot_is_new("cenc", Some("a"), &revisions));
        assert!(snapshot_is_new("cenc", Some("b"), &revisions));
        assert!(snapshot_is_new("cenc", None, &revisions));
    }

    #[test]
    fn rejects_unknown_update_cursor_sources() {
        let unknown = serde_json::json!({"source":"unknown","md5":"a"});
        assert!(update_cursor(&unknown).is_err());
    }

    #[test]
    fn fanstudio_registry_streams_are_unique() {
        let all_streams = SOURCES
            .iter()
            .filter(|source| source.channel == ProviderChannel::FanStudio)
            .map(|source| source.provider_key)
            .collect::<Vec<_>>();
        let streams = all_streams
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert!(!streams.is_empty());
        assert_eq!(streams.len(), all_streams.len());
    }
}
