use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use disaster_alert::migration_support::{MigrationStorage, Subscription};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use zeroize::Zeroizing;

const SUBSCRIPTION_PREFIX: &[u8] = b"sub:";
const MAX_SUBSCRIPTION_BYTES: usize = 32 * 1024;
const MIGRATION_BATCH_SIZE: usize = 1_000;

#[derive(Debug)]
struct SourceSnapshot {
    subscriptions: Vec<Subscription>,
    invalid: Vec<InvalidRecord>,
    fingerprint: [u8; 32],
    digest: [u8; 32],
}

#[derive(Debug, Serialize)]
struct InvalidRecord {
    record: String,
    error: String,
}

fn main() -> Result<()> {
    let (source, target) = migration_paths()?;
    migrate(&source, &target, data_encryption_key_from_env()?)
}

fn data_encryption_key_from_env() -> Result<[u8; 32]> {
    let encoded =
        Zeroizing::new(env::var("DATA_ENCRYPTION_KEY").context("DATA_ENCRYPTION_KEY is required")?);
    let decoded = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(encoded.trim().as_bytes())
            .context("DATA_ENCRYPTION_KEY must be URL-safe base64 without padding")?,
    );
    anyhow::ensure!(
        decoded.len() == 32,
        "DATA_ENCRYPTION_KEY must decode to exactly 32 bytes"
    );
    let mut key = [0_u8; 32];
    key.copy_from_slice(&decoded);
    Ok(key)
}

fn migration_paths() -> Result<(PathBuf, PathBuf)> {
    migration_paths_from(env::args_os())
}

fn migration_paths_from(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<(PathBuf, PathBuf)> {
    let executable = arguments
        .next()
        .unwrap_or_else(|| "disaster-alert-migrate".into());
    let usage = || {
        format!(
            "usage: {} <sled-source-directory> <fjall-target-directory>",
            Path::new(&executable)
                .file_name()
                .unwrap_or(executable.as_os_str())
                .to_string_lossy()
        )
    };
    let source = arguments.next().context(usage())?;
    let target = arguments.next().context(usage())?;
    anyhow::ensure!(arguments.next().is_none(), usage());
    Ok((PathBuf::from(source), PathBuf::from(target)))
}

fn migrate(source: &Path, target: &Path, data_encryption_key: [u8; 32]) -> Result<()> {
    anyhow::ensure!(source.is_dir(), "sled source must be an existing directory");
    anyhow::ensure!(
        !target.exists(),
        "target already exists: {}",
        target.display()
    );
    let backup = sibling_with_suffix(source, ".migration-backup");
    ensure_non_overlapping_paths(source, target, &backup)?;
    prepare_backup(source, &backup)?;
    let snapshot = scan_sled(&backup)?;
    if !snapshot.invalid.is_empty() {
        let quarantine = sibling_with_suffix(target, ".quarantine.jsonl");
        write_quarantine(&quarantine, &snapshot.invalid)?;
        anyhow::bail!(
            "source contains {} invalid subscriptions; quarantine report: {}",
            snapshot.invalid.len(),
            quarantine.display()
        );
    }

    let partial = sibling_with_suffix(target, ".partial");
    let partial_existed = partial.exists();
    if let Some(parent) = partial.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create target parent {}", parent.display()))?;
    }
    let target_storage = MigrationStorage::open(&partial, data_encryption_key)
        .with_context(|| format!("failed to open Fjall partial {}", partial.display()))?;
    target_storage.bind_source(snapshot.fingerprint, partial_existed)?;
    let mut imported = 0usize;
    for chunk in snapshot.subscriptions.chunks(MIGRATION_BATCH_SIZE) {
        let mut missing = Vec::with_capacity(chunk.len());
        for subscription in chunk {
            match target_storage.subscription(&subscription.destination_id())? {
                Some(current) => anyhow::ensure!(
                    canonical_json(&current)? == canonical_json(subscription)?,
                    "partial target contains a different subscription"
                ),
                None => missing.push(subscription.clone()),
            }
        }
        if !missing.is_empty() {
            imported = imported.saturating_add(target_storage.import_subscriptions(missing)?);
        }
    }
    target_storage.flush()?;
    drop(target_storage);

    let report = verify_snapshot(&snapshot, &partial, data_encryption_key)?;
    sync_tree(&partial)?;
    fs::rename(&partial, target).with_context(|| {
        format!(
            "failed to atomically promote {} to {}",
            partial.display(),
            target.display()
        )
    })?;
    sync_parent(target)?;
    writeln!(
        io::stdout().lock(),
        "migrated={} reused={} target={} digest={}",
        imported,
        snapshot.subscriptions.len().saturating_sub(imported),
        target.display(),
        hex(&report.digest)
    )?;
    Ok(())
}

struct VerifyReport {
    digest: [u8; 32],
}

fn verify_snapshot(
    source: &SourceSnapshot,
    target: &Path,
    data_encryption_key: [u8; 32],
) -> Result<VerifyReport> {
    anyhow::ensure!(
        target.is_dir(),
        "Fjall target must be an existing directory"
    );
    anyhow::ensure!(
        source.invalid.is_empty(),
        "source contains invalid subscriptions"
    );
    let target = MigrationStorage::open(target, data_encryption_key)
        .with_context(|| format!("failed to open Fjall target {}", target.display()))?;
    target.verify_source(source.fingerprint)?;
    target.verify_postings()?;
    target.verify_matches(&source.subscriptions)?;
    let target_subscriptions = target.subscriptions()?;
    anyhow::ensure!(
        target_subscriptions.len() == source.subscriptions.len(),
        "Fjall target subscription count differs from source"
    );
    let target_digest = subscriptions_digest(&target_subscriptions)?;
    anyhow::ensure!(
        target_digest == source.digest,
        "Fjall target normalized subscription digest differs from source"
    );
    target.flush()?;
    drop(target);
    Ok(VerifyReport {
        digest: source.digest,
    })
}

fn load_source_read_only(path: &Path) -> Result<SourceSnapshot> {
    anyhow::ensure!(path.is_dir(), "sled source must be a directory");
    let snapshot = sibling_with_suffix(path, &format!(".read-snapshot-{}", std::process::id()));
    anyhow::ensure!(
        !snapshot.exists(),
        "temporary sled snapshot already exists: {}",
        snapshot.display()
    );
    copy_directory(path, &snapshot)?;
    let result = scan_sled(&snapshot);
    let cleanup = fs::remove_dir_all(&snapshot)
        .with_context(|| format!("failed to remove {}", snapshot.display()));
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(error.context(cleanup_error.to_string())),
    }
}

fn scan_sled(path: &Path) -> Result<SourceSnapshot> {
    anyhow::ensure!(path.is_dir(), "sled source must be a directory");
    let database = sled::open(path)
        .with_context(|| format!("failed to open sled snapshot {}", path.display()))?;
    let mut subscriptions = Vec::new();
    let mut invalid = Vec::new();
    let mut fingerprint = Sha256::new();
    fingerprint.update(b"disaster-alert:migration-source:sled:v1\0");
    for item in database.iter() {
        let (key, value) = item.context("failed to scan sled snapshot")?;
        if !key.starts_with(SUBSCRIPTION_PREFIX) {
            continue;
        }
        hash_record(&mut fingerprint, &key, &value);
        match parse_sled_record(&key, &value) {
            Ok(subscription) => {
                subscriptions.push(subscription);
            }
            Err(error) => {
                invalid.push(InvalidRecord {
                    record: redact_legacy_key(&key),
                    error: format!("{error:#}"),
                });
            }
        }
    }
    drop(database);
    finish_snapshot(subscriptions, invalid, fingerprint.finalize().into())
}

fn finish_snapshot(
    subscriptions: Vec<Subscription>,
    invalid: Vec<InvalidRecord>,
    fingerprint: [u8; 32],
) -> Result<SourceSnapshot> {
    let digest = subscriptions_digest(&subscriptions)?;
    Ok(SourceSnapshot {
        subscriptions,
        invalid,
        fingerprint,
        digest,
    })
}

fn parse_sled_record(key: &[u8], value: &[u8]) -> Result<Subscription> {
    anyhow::ensure!(
        value.len() <= MAX_SUBSCRIPTION_BYTES,
        "subscription record is oversized"
    );
    let subscription: Subscription =
        serde_json::from_slice(value).context("invalid subscription JSON")?;
    validate_subscription(&subscription)?;
    anyhow::ensure!(
        key == legacy_key(&subscription),
        "subscription key/value identity mismatch"
    );
    Ok(subscription)
}

fn validate_subscription(subscription: &Subscription) -> Result<()> {
    subscription
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid subscription: {error}"))
}

fn subscriptions_digest(subscriptions: &[Subscription]) -> Result<[u8; 32]> {
    let mut records = subscriptions
        .iter()
        .map(|subscription| Ok((legacy_key(subscription), canonical_json(subscription)?)))
        .collect::<Result<Vec<_>>>()?;
    records.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    for pair in records.windows(2) {
        anyhow::ensure!(
            pair[0].0 != pair[1].0,
            "source contains duplicate destinations"
        );
    }
    let mut hash = Sha256::new();
    hash.update(b"disaster-alert:migration-subscriptions:v1\0");
    for (key, value) in records {
        hash_record(&mut hash, &key, &value);
    }
    Ok(hash.finalize().into())
}

fn canonical_json(subscription: &Subscription) -> Result<Vec<u8>> {
    serde_json::to_vec(subscription).context("failed to encode subscription for verification")
}

fn hash_record(hash: &mut Sha256, key: &[u8], value: &[u8]) {
    hash.update(u64::try_from(key.len()).unwrap_or(u64::MAX).to_be_bytes());
    hash.update(key);
    hash.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hash.update(value);
}

fn prepare_backup(source: &Path, backup: &Path) -> Result<()> {
    if backup.exists() {
        let source_snapshot = load_source_read_only(source)?;
        let backup_snapshot = scan_sled(backup)?;
        anyhow::ensure!(
            source_snapshot.fingerprint == backup_snapshot.fingerprint,
            "backup does not match the requested source database"
        );
        return Ok(());
    }
    let partial = sibling_with_suffix(backup, ".partial");
    if partial.exists() {
        remove_path(&partial)?;
    }
    if let Some(parent) = partial.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    copy_directory(source, &partial)?;
    fs::rename(&partial, backup).with_context(|| {
        format!(
            "failed to atomically promote backup {} to {}",
            partial.display(),
            backup.display()
        )
    })?;
    sync_parent(backup)
}

fn remove_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect temporary path {}", path.display()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("failed to remove temporary path {}", path.display()))
}

fn write_quarantine(path: &Path, invalid: &[InvalidRecord]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)
        .with_context(|| format!("failed to create quarantine report {}", path.display()))?;
    for record in invalid {
        serde_json::to_writer(&mut file, record)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    sync_parent(path)
}

fn legacy_key(subscription: &Subscription) -> Vec<u8> {
    format!(
        "sub:{}:{}:{}",
        subscription.bark_base_url().len(),
        subscription.bark_base_url(),
        subscription.device_key()
    )
    .into_bytes()
}

fn copy_directory(source: &Path, target: &Path) -> Result<()> {
    anyhow::ensure!(source.is_dir(), "source directory does not exist");
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &destination)?;
            sync_file(&destination)?;
        } else {
            anyhow::bail!("source contains unsupported filesystem entries");
        }
    }
    sync_directory(target)
}

fn sync_tree(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            sync_file(&entry.path())?;
        }
    }
    sync_directory(path)
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    sync_directory(parent)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open directory {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

fn ensure_non_overlapping_paths(source: &Path, target: &Path, backup: &Path) -> Result<()> {
    let candidates = [
        ("source", resolve_overlap_path(source)?),
        ("Fjall target", resolve_overlap_path(target)?),
        (
            "Fjall partial target",
            resolve_overlap_path(&sibling_with_suffix(target, ".partial"))?,
        ),
        ("source backup", resolve_overlap_path(backup)?),
        (
            "partial source backup",
            resolve_overlap_path(&sibling_with_suffix(backup, ".partial"))?,
        ),
    ];
    for (index, (left_label, left_path)) in candidates.iter().enumerate() {
        for (right_label, right_path) in candidates.iter().skip(index + 1) {
            anyhow::ensure!(
                !paths_overlap(left_path, right_path),
                "{left_label} path overlaps {right_label}: {} <-> {}",
                left_path.display(),
                right_path.display()
            );
        }
    }
    Ok(())
}

fn resolve_overlap_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .context("failed to read current working directory")?
            .join(path)
    };
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .with_context(|| format!("path {} has no existing ancestor", path.display()))?;
        missing.push(name.to_os_string());
        existing = existing
            .parent()
            .with_context(|| format!("path {} has no existing ancestor", path.display()))?;
    }
    let mut resolved = existing
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", existing.display()))?;
    for segment in missing.iter().rev() {
        resolved.push(segment);
    }
    Ok(normalize_path(resolved))
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn redact_legacy_key(key: &[u8]) -> String {
    let Some((length, base_url, _device_key)) = parse_legacy_key_segments(key) else {
        return format!("sha256:{}", hex(&Sha256::digest(key)));
    };
    format!(
        "sub:{length}:sha256:{}:****",
        hex(&Sha256::digest(base_url))
    )
}

fn parse_legacy_key_segments(key: &[u8]) -> Option<(usize, &[u8], &[u8])> {
    let remainder = key.strip_prefix(SUBSCRIPTION_PREFIX)?;
    let separator = remainder.iter().position(|byte| *byte == b':')?;
    let length = std::str::from_utf8(&remainder[..separator])
        .ok()?
        .parse::<usize>()
        .ok()?;
    let base_start = separator.checked_add(1)?;
    let base_end = base_start.checked_add(length)?;
    if remainder.get(base_end) != Some(&b':') {
        return None;
    }
    let base = remainder.get(base_start..base_end)?;
    let device = remainder.get(base_end.checked_add(1)?..)?;
    (!base.is_empty() && !device.is_empty()).then_some((length, base, device))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use disaster_alert::migration_support::{
        AlertRule, DisasterCategory, GeoPoint, MonitoringTarget, NotificationDestination,
    };

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
            vec![AlertRule::default_for(DisasterCategory::EarthquakeReport)],
        )
    }

    #[test]
    fn sled_migration_imports_only_subscriptions() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source_path = directory.path().join("old-sled");
        let target_path = directory.path().join("new-fjall");
        let source = sled::open(&source_path)?;
        let value = subscription();
        source.insert(legacy_key(&value), serde_json::to_vec(&value)?)?;
        source.insert(b"incident:ignored", b"legacy")?;
        source.flush()?;
        drop(source);
        migrate(&source_path, &target_path, [7; 32])?;
        let snapshot = load_source_read_only(&source_path)?;
        let report = verify_snapshot(&snapshot, &target_path, [7; 32])?;
        anyhow::ensure!(snapshot.subscriptions.len() == 1);
        anyhow::ensure!(report.digest == snapshot.digest);
        Ok(())
    }

    #[test]
    fn migration_rejects_non_directory_source() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("new-fjall");
        fs::write(&source_path, b"not a sled directory")?;

        anyhow::ensure!(migrate(&source_path, &target_path, [7; 32]).is_err());
        anyhow::ensure!(!target_path.exists());
        Ok(())
    }

    #[test]
    fn existing_partial_is_resumed_without_a_command_flag() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source_path = directory.path().join("old-sled");
        let target_path = directory.path().join("new-fjall");
        let source = sled::open(&source_path)?;
        let value = subscription();
        source.insert(legacy_key(&value), serde_json::to_vec(&value)?)?;
        source.flush()?;
        drop(source);

        let partial = sibling_with_suffix(&target_path, ".partial");
        drop(MigrationStorage::open(&partial, [7; 32])?);
        migrate(&source_path, &target_path, [7; 32])?;

        let snapshot = load_source_read_only(&source_path)?;
        anyhow::ensure!(
            verify_snapshot(&snapshot, &target_path, [7; 32])?.digest == snapshot.digest
        );
        Ok(())
    }

    #[test]
    fn interrupted_source_snapshot_is_rebuilt_automatically() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source_path = directory.path().join("old-sled");
        let target_path = directory.path().join("new-fjall");
        let source = sled::open(&source_path)?;
        let value = subscription();
        source.insert(legacy_key(&value), serde_json::to_vec(&value)?)?;
        source.flush()?;
        drop(source);

        let backup = sibling_with_suffix(&source_path, ".migration-backup");
        let interrupted = sibling_with_suffix(&backup, ".partial");
        fs::create_dir(&interrupted)?;
        fs::write(interrupted.join("incomplete"), b"partial copy")?;

        migrate(&source_path, &target_path, [7; 32])?;

        anyhow::ensure!(target_path.is_dir());
        anyhow::ensure!(!interrupted.exists());
        Ok(())
    }

    #[test]
    fn invalid_source_writes_a_quarantine_report() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source_path = directory.path().join("old-sled");
        let target_path = directory.path().join("new-fjall");
        let source = sled::open(&source_path)?;
        source.insert(b"sub:1:x:key", b"not-json")?;
        source.flush()?;
        drop(source);
        anyhow::ensure!(migrate(&source_path, &target_path, [7; 32]).is_err());
        anyhow::ensure!(sibling_with_suffix(&target_path, ".quarantine.jsonl").is_file());
        anyhow::ensure!(!target_path.exists());
        Ok(())
    }

    #[test]
    fn overlap_check_rejects_partial_target_collision() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source_path = directory.path().join("old-sled");
        fs::create_dir(&source_path)?;
        let target_path = directory.path().join("new-fjall");
        let backup_path = sibling_with_suffix(&target_path, ".partial");
        anyhow::ensure!(
            ensure_non_overlapping_paths(&source_path, &target_path, &backup_path).is_err()
        );
        Ok(())
    }

    #[test]
    fn legacy_key_redaction_never_slices_untrusted_utf8() {
        assert!(redact_legacy_key(b"sub:5:abc").starts_with("sha256:"));
        assert!(redact_legacy_key(b"sub:3:\xff\xff\xff:\xff").ends_with(":****"));
        assert!(!redact_legacy_key(b"sub:3:\xff\xff\xff:\xff").contains('\u{fffd}'));
    }

    #[test]
    fn command_accepts_only_source_and_target_paths() -> Result<()> {
        let arguments = ["disaster-alert-migrate", "old-sled", "new-fjall"]
            .into_iter()
            .map(OsString::from);
        let (source, target) = migration_paths_from(arguments)?;
        anyhow::ensure!(source == PathBuf::from("old-sled"));
        anyhow::ensure!(target == PathBuf::from("new-fjall"));

        let legacy_arguments = ["disaster-alert-migrate", "migrate", "old-sled", "new-fjall"]
            .into_iter()
            .map(OsString::from);
        anyhow::ensure!(migration_paths_from(legacy_arguments).is_err());
        Ok(())
    }
}
