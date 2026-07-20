mod codec;
mod encryption;
mod facade;
mod fjall;

pub(crate) use codec::{decode_record, encode_record};
pub(crate) use encryption::RecordCipher;
pub(crate) use facade::{BacklogCounts, RetentionPolicy, Storage};
pub(crate) use fjall::{FjallStorage, InboxItem, IncidentResolutionCapacity};

pub(crate) fn try_now_millis() -> anyhow::Result<i64> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| anyhow::anyhow!("system clock is before the Unix epoch: {error}"))?;
    i64::try_from(duration.as_millis())
        .map_err(|error| anyhow::anyhow!("system clock exceeds the supported range: {error}"))
}
