//! CLI-owned composition of CCR over Tiny Storage Drivers' file backend.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tinyjuice::cache::{CcrPutResult, CcrStore, set_durable_tier};
use tinyjuice_drivers::{BlobCcrStore, CCR_PREFIX};
use tinystoragedrivers::{Scope, StorageConfig, open};
use tinystoragedrivers_api::BlobStore;
use tinystoragedrivers_types::BlobMeta;

const PAGE_SIZE: usize = 1000;
const DEFAULT_CLI_CCR_TTL_SECS: u64 = 7 * 24 * 60 * 60;
// The blob port has no caller metadata field. Keep legacy/file write chronology
// in a sibling key so the raw CCR body and its content token remain unchanged.
const CCR_METADATA_PREFIX: &str = "tinyjuice/ccr-metadata/";
const CCR_METADATA_CONTENT_TYPE: &str = "application/vnd.tinyjuice.ccr-metadata";
const CCR_IMPORT_PREFIX: &str = "tinyjuice/ccr-import-intent/";
const CCR_STORAGE_CONFIGURATION_KEY: &str = "tinyjuice/ccr-config/applied-storage";

#[allow(dead_code)] // The reducer binary uses inventory; the hook binary shares this module.
pub(crate) struct Inventory {
    pub entries: Vec<BlobMeta>,
    pub ignored_entries: usize,
}

struct PruneReport {
    removed: usize,
    bytes_freed: u64,
    kept: usize,
    kept_bytes: u64,
}

struct ConfigureReport {
    legacy_removed: usize,
    legacy_bytes: u64,
    pruned: PruneReport,
}

pub(crate) fn configure(
    root: &Path,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
) -> Result<(), String> {
    configure_with_report(root, ttl, max_bytes).map(|_| ())
}

fn configure_with_report(
    root: &Path,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
) -> Result<ConfigureReport, String> {
    std::fs::create_dir_all(root).map_err(|error| {
        format!(
            "cannot create CCR storage directory {}: {error}",
            root.display()
        )
    })?;
    let blobs = open_file_blobs(root)?;
    let _lock = acquire_process_lock(&process_lock_path(root))?;
    let discarded_legacy = import_legacy_files(root, &blobs, ttl)?;
    let store = BlobCcrStore::new(Arc::clone(&blobs), ttl).map_err(|error| error.to_string())?;
    reapply_existing_blob_ttl_if_changed(&blobs, ttl, max_bytes)?;
    let capped = CappedCcrStore {
        store,
        blobs: Arc::clone(&blobs),
        lock_path: process_lock_path(root),
        ttl,
        max_bytes,
    };
    let pruned = if ttl.is_some() || max_bytes.is_some() {
        let (removed, bytes_freed, kept, kept_bytes) = prune(&blobs, ttl, max_bytes, None)?;
        PruneReport {
            removed,
            bytes_freed,
            kept,
            kept_bytes,
        }
    } else {
        PruneReport {
            removed: 0,
            bytes_freed: 0,
            kept: 0,
            kept_bytes: 0,
        }
    };
    set_durable_tier(Some(Arc::new(capped)));
    Ok(ConfigureReport {
        legacy_removed: discarded_legacy.0,
        legacy_bytes: discarded_legacy.1,
        pruned,
    })
}

#[allow(dead_code)] // The reducer binary uses inventory; the hook binary shares this module.
pub(crate) fn inventory(root: &Path, ttl: Option<Duration>) -> Result<Inventory, String> {
    if !root.is_dir() {
        return Err(format!(
            "CCR storage directory does not exist: {}",
            root.display()
        ));
    }
    let mut ignored_entries = 0;
    for entry in std::fs::read_dir(root).map_err(|error| {
        format!(
            "failed to inspect CCR storage directory {}: {error}",
            root.display()
        )
    })? {
        let entry = entry.map_err(|error| format!("failed to inspect CCR entry: {error}"))?;
        if entry.file_name() == "scopes" {
            continue;
        }
        if !entry
            .file_type()
            .map_err(|error| format!("failed to inspect CCR entry type: {error}"))?
            .is_file()
            || !is_token(&entry.file_name().to_string_lossy())
        {
            ignored_entries += 1;
        }
    }
    configure(root, ttl, None)?;
    let blobs = open_file_blobs(root)?;
    let mut entries = Vec::new();
    let mut after = None;
    loop {
        let page = run_blob({
            let after = after.clone();
            let blobs = Arc::clone(&blobs);
            async move {
                blobs
                    .list_page(&format!("{CCR_PREFIX}/"), after.as_deref(), PAGE_SIZE)
                    .await
            }
        })?;
        entries.extend(page.items.into_iter().map(|item| item.meta));
        after = page.next_after;
        if after.is_none() {
            break;
        }
    }
    Ok(Inventory {
        entries,
        ignored_entries,
    })
}

#[allow(dead_code)] // The reducer binary shares this storage module.
pub(crate) fn configured_ttl() -> Option<Duration> {
    let seconds = std::env::var("TINYJUICE_CCR_TTL_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_CLI_CCR_TTL_SECS);
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

#[allow(dead_code)] // The hook binary shares this CLI-only collector.
pub(crate) fn collect(
    root: &Path,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
) -> Result<(usize, u64, usize, u64), String> {
    let report = configure_with_report(root, ttl, max_bytes)?;
    let pruned = if ttl.is_some() || max_bytes.is_some() {
        report.pruned
    } else {
        let blobs = open_file_blobs(root)?;
        let remaining = list_all(blobs)?;
        PruneReport {
            removed: 0,
            bytes_freed: 0,
            kept: remaining.len(),
            kept_bytes: remaining.iter().map(|entry| entry.meta.len).sum(),
        }
    };
    Ok((
        report.legacy_removed.saturating_add(pruned.removed),
        report.legacy_bytes.saturating_add(pruned.bytes_freed),
        pruned.kept,
        pruned.kept_bytes,
    ))
}

fn prune(
    blobs: &Arc<dyn BlobStore>,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
    protect: Option<&str>,
) -> Result<(usize, u64, usize, u64), String> {
    run_blob({
        let blobs = Arc::clone(blobs);
        let protect = protect.map(str::to_owned);
        async move { prune_async(blobs, ttl, max_bytes, protect.as_deref()).await }
    })
}

fn reapply_existing_blob_ttl_if_changed(
    blobs: &Arc<dyn BlobStore>,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
) -> Result<(), String> {
    let ttl_signature = ttl_signature(ttl);
    let signature = storage_configuration_signature(ttl, max_bytes);
    let key = CCR_STORAGE_CONFIGURATION_KEY.to_owned();
    let existing = run_blob({
        let blobs = Arc::clone(blobs);
        let key = key.clone();
        async move { blobs.get(&key).await }
    })?;
    let existing_signature = existing.map(|blob| String::from_utf8_lossy(&blob.bytes).into_owned());
    let existing_ttl_signature = existing_signature
        .as_deref()
        .map(|value| value.split('|').next().unwrap_or(value));
    if existing_ttl_signature != Some(ttl_signature.as_str()) {
        reapply_existing_blob_ttl(blobs, ttl)?;
    }

    if existing_signature.as_deref() == Some(signature.as_str()) {
        return Ok(());
    }
    run_blob({
        let blobs = Arc::clone(blobs);
        async move {
            blobs
                .put(&key, signature.into_bytes(), None)
                .await
                .map(|_| ())
        }
    })
}

fn ttl_signature(ttl: Option<Duration>) -> String {
    match ttl {
        Some(ttl) => format!("{}.{:09}", ttl.as_secs(), ttl.subsec_nanos()),
        None => "disabled".to_owned(),
    }
}

fn storage_configuration_signature(ttl: Option<Duration>, max_bytes: Option<u64>) -> String {
    let max_bytes = max_bytes.map_or_else(|| "unlimited".to_owned(), |value| value.to_string());
    format!("{}|{max_bytes}", ttl_signature(ttl))
}

fn stored_configuration_matches(
    blobs: &Arc<dyn BlobStore>,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
) -> Result<bool, String> {
    let key = CCR_STORAGE_CONFIGURATION_KEY.to_owned();
    let signature = storage_configuration_signature(ttl, max_bytes);
    let existing = run_blob({
        let blobs = Arc::clone(blobs);
        async move { blobs.get(&key).await }
    })?;
    Ok(existing.is_none_or(|blob| blob.bytes == signature.as_bytes()))
}

fn reapply_existing_blob_ttl(
    blobs: &Arc<dyn BlobStore>,
    ttl: Option<Duration>,
) -> Result<(), String> {
    run_blob({
        let blobs = Arc::clone(blobs);
        async move {
            let body_prefix = format!("{CCR_PREFIX}/");
            let bodies = list_all_prefix_async(Arc::clone(&blobs), &body_prefix).await?;
            let semantic_times =
                list_timestamps_async(Arc::clone(&blobs), CCR_METADATA_PREFIX).await?;
            let now = SystemTime::now();

            for entry in bodies {
                let Some(token) = entry.meta.key.strip_prefix(&body_prefix) else {
                    continue;
                };
                let Some(body) = blobs
                    .get(&entry.meta.key)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    continue;
                };
                let written_at_ms = semantic_times
                    .get(token)
                    .copied()
                    .or(entry.written_at_ms)
                    .unwrap_or_else(|| unix_epoch_ms(now));
                if !semantic_times.contains_key(token) {
                    refresh_blob_expiry_async(
                        Arc::clone(&blobs),
                        &format!("{CCR_METADATA_PREFIX}{token}"),
                        written_at_ms.to_string().into_bytes(),
                        Some(CCR_METADATA_CONTENT_TYPE),
                        Some(written_at_ms),
                        ttl,
                        now,
                    )
                    .await?;
                }
                refresh_blob_expiry_async(
                    Arc::clone(&blobs),
                    &entry.meta.key,
                    body.bytes,
                    body.meta.content_type.as_deref(),
                    Some(written_at_ms),
                    ttl,
                    now,
                )
                .await?;
            }

            for prefix in [CCR_METADATA_PREFIX, CCR_IMPORT_PREFIX] {
                let entries = list_all_prefix_async(Arc::clone(&blobs), prefix).await?;
                for entry in entries {
                    let Some(token) = entry.meta.key.strip_prefix(prefix) else {
                        continue;
                    };
                    if !is_token(token) {
                        continue;
                    }
                    let Some(sidecar) = blobs
                        .get(&entry.meta.key)
                        .await
                        .map_err(|error| error.to_string())?
                    else {
                        continue;
                    };
                    let Ok(written_at_ms) = parse_timestamp(&sidecar.bytes) else {
                        continue;
                    };
                    refresh_blob_expiry_async(
                        Arc::clone(&blobs),
                        &entry.meta.key,
                        sidecar.bytes,
                        sidecar.meta.content_type.as_deref(),
                        Some(written_at_ms),
                        ttl,
                        now,
                    )
                    .await?;
                }
            }
            Ok::<(), String>(())
        }
    })
}

async fn refresh_blob_expiry_async(
    blobs: Arc<dyn BlobStore>,
    key: &str,
    bytes: Vec<u8>,
    content_type: Option<&str>,
    written_at_ms: Option<u64>,
    ttl: Option<Duration>,
    now: SystemTime,
) -> Result<(), String> {
    match ttl {
        None => blobs
            .put(key, bytes, content_type)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string()),
        Some(ttl) => {
            let remaining = match written_at_ms {
                Some(written) => remaining_ttl_from_epoch_ms(written, now, ttl),
                None => Some(ttl),
            };
            let Some(remaining) = remaining else {
                return Ok(());
            };
            if remaining.is_zero() {
                return Ok(());
            }
            blobs
                .put_expiring(key, bytes, content_type, remaining)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
    }
}

async fn prune_async(
    blobs: Arc<dyn BlobStore>,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
    protect: Option<&str>,
) -> Result<(usize, u64, usize, u64), String> {
    let mut entries = list_all_prefix_async(Arc::clone(&blobs), &format!("{CCR_PREFIX}/")).await?;
    let semantic_times = list_timestamps_async(Arc::clone(&blobs), CCR_METADATA_PREFIX).await?;
    for entry in &mut entries {
        if let Some(token) = entry.meta.key.strip_prefix(&format!("{CCR_PREFIX}/"))
            && let Some(written_at_ms) = semantic_times.get(token)
        {
            entry.written_at_ms = Some(*written_at_ms);
        }
    }
    let now_ms = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let ttl_ms = ttl.map(|ttl| ttl.as_millis().min(u64::MAX as u128) as u64);
    let mut remove = Vec::new();
    let mut removed_bytes = 0_u64;
    for entry in &entries {
        if ttl_ms.is_some_and(|ttl| {
            entry
                .written_at_ms
                .is_some_and(|written| now_ms.saturating_sub(written) >= ttl)
        }) {
            remove.push(entry.meta.key.clone());
        }
    }
    for key in &remove {
        if let Some(entry) = entries.iter().find(|entry| &entry.meta.key == key) {
            removed_bytes = removed_bytes.saturating_add(entry.meta.len);
        }
        delete_blob_and_metadata_async(Arc::clone(&blobs), key).await?;
    }
    entries.retain(|entry| !remove.contains(&entry.meta.key));
    let mut total = entries.iter().map(|entry| entry.meta.len).sum::<u64>();
    if let Some(budget) = max_bytes {
        entries.sort_by_key(|entry| entry.written_at_ms.unwrap_or(0));
        for entry in entries {
            if total <= budget {
                break;
            }
            if protect.is_some_and(|token| entry.meta.key == format!("{CCR_PREFIX}/{token}")) {
                continue;
            }
            delete_blob_and_metadata_async(Arc::clone(&blobs), &entry.meta.key).await?;
            removed_bytes = removed_bytes.saturating_add(entry.meta.len);
            total = total.saturating_sub(entry.meta.len);
            remove.push(entry.meta.key);
        }
    }
    let remaining = list_all_prefix_async(Arc::clone(&blobs), &format!("{CCR_PREFIX}/")).await?;
    let remaining_bytes = remaining.iter().map(|entry| entry.meta.len).sum();
    let remaining_tokens = remaining
        .iter()
        .filter_map(|entry| entry.meta.key.strip_prefix(&format!("{CCR_PREFIX}/")))
        .collect::<HashSet<_>>();
    for token in semantic_times.keys() {
        if !remaining_tokens.contains(token.as_str()) {
            delete_metadata_async(Arc::clone(&blobs), token).await?;
        }
    }
    for token in list_timestamps_async(Arc::clone(&blobs), CCR_IMPORT_PREFIX)
        .await?
        .keys()
    {
        if !remaining_tokens.contains(token.as_str()) {
            delete_import_intent_async(Arc::clone(&blobs), token).await?;
        }
    }
    Ok((
        remove.len(),
        removed_bytes,
        remaining.len(),
        remaining_bytes,
    ))
}

fn open_file_blobs(root: &Path) -> Result<Arc<dyn BlobStore>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start storage runtime: {error}"))?;
    let url = format!("file:{}", root.display());
    let config = StorageConfig::parse(&url).map_err(|error| error.to_string())?;
    let backend = runtime
        .block_on(open(&config))
        .map_err(|error| error.to_string())?;
    let scope = Scope::new("tinyjuice-cli").map_err(|error| error.to_string())?;
    let storage = backend
        .for_scope(&scope)
        .map_err(|error| error.to_string())?;
    let blobs = Arc::clone(storage.blobs());
    drop(runtime);
    Ok(blobs)
}

fn import_legacy_files(
    root: &Path,
    blobs: &Arc<dyn BlobStore>,
    ttl: Option<Duration>,
) -> Result<(usize, u64), String> {
    let mut removed_legacy = 0_usize;
    let mut removed_legacy_bytes = 0_u64;
    let entries = std::fs::read_dir(root).map_err(|error| {
        format!(
            "failed to inspect legacy CCR directory {}: {error}",
            root.display()
        )
    })?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to inspect legacy CCR entry: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            continue;
        }
        let token = entry.file_name().to_string_lossy().into_owned();
        if !is_token(&token) {
            continue;
        }
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                log::warn!(
                    "[tinyjuice_cli] discarding non-UTF-8 legacy CCR entry {token}: {error}"
                );
                if let Some(size) = remove_legacy_entry(&entry.path(), &token, "non-UTF-8")? {
                    removed_legacy = removed_legacy.saturating_add(1);
                    removed_legacy_bytes = removed_legacy_bytes.saturating_add(size);
                }
                continue;
            }
            Err(error) => {
                log::warn!("[tinyjuice_cli] skipping unreadable legacy CCR entry {token}: {error}");
                continue;
            }
        };
        if tinyjuice::cache::short_hash(&content) != token {
            log::warn!("[tinyjuice_cli] skipping legacy CCR entry {token}: content hash mismatch");
            if let Some(size) = remove_legacy_entry(&entry.path(), &token, "corrupt")? {
                removed_legacy = removed_legacy.saturating_add(1);
                removed_legacy_bytes = removed_legacy_bytes.saturating_add(size);
            }
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map_err(|error| {
                format!("cannot read modification time for legacy CCR entry {token}: {error}")
            })?;
        let source_written_at_ms = unix_epoch_ms(modified);
        let import_ttl = match ttl {
            Some(ttl) => {
                let remaining = remaining_ttl(modified, SystemTime::now(), ttl);
                if remaining.is_none() {
                    if let Some(size) = remove_legacy_entry(&entry.path(), &token, "expired")? {
                        removed_legacy = removed_legacy.saturating_add(1);
                        removed_legacy_bytes = removed_legacy_bytes.saturating_add(size);
                    }
                    continue;
                }
                remaining
            }
            None => None,
        };
        let store =
            BlobCcrStore::new(Arc::clone(blobs), import_ttl).map_err(|error| error.to_string())?;
        let existing = store.get(&token);
        if existing
            .as_deref()
            .is_some_and(|existing| existing != content)
        {
            return Err(format!(
                "legacy CCR entry {token} conflicts with different stored content"
            ));
        }
        let recorded_time = read_semantic_time(Arc::clone(blobs), &token)?;
        let import_intent = read_import_intent(Arc::clone(blobs), &token)?;
        let existing_written_at_ms = if existing.is_some() {
            recorded_time.or(import_intent).or(backend_written_at(
                Arc::clone(blobs),
                &format!("{CCR_PREFIX}/{token}"),
            )?)
        } else {
            import_intent
        };
        let semantic_written_at_ms = existing_written_at_ms
            .filter(|existing| *existing >= source_written_at_ms)
            .unwrap_or(source_written_at_ms);
        if existing.is_none() && import_intent.is_none() {
            // Mark source chronology before publishing its body. If the body
            // succeeds but the timestamp write is interrupted, retry can
            // distinguish this partial import from a newer normal store write.
            write_import_intent(Arc::clone(blobs), &token, source_written_at_ms, import_ttl)?;
        }
        if existing.is_none() || semantic_written_at_ms == source_written_at_ms {
            let put = store.put(&content);
            if put.token() != token || !put.retained() {
                return Err(format!("legacy CCR entry {token} was not durably retained"));
            }
        }
        if store.get(&token).as_deref() != Some(content.as_str()) {
            return Err(format!("legacy CCR entry {token} failed content readback"));
        }
        let metadata_ttl = ttl.and_then(|ttl| {
            remaining_ttl_from_epoch_ms(semantic_written_at_ms, SystemTime::now(), ttl)
        });
        write_semantic_time(
            Arc::clone(blobs),
            &token,
            semantic_written_at_ms,
            metadata_ttl,
        )?;
        if read_semantic_time(Arc::clone(blobs), &token)? != Some(semantic_written_at_ms) {
            return Err(format!(
                "legacy CCR entry {token} failed timestamp readback"
            ));
        }
        delete_import_intent(Arc::clone(blobs), &token)?;
        remove_imported_legacy_entry(&entry.path(), &token)?;
    }
    Ok((removed_legacy, removed_legacy_bytes))
}

fn remove_imported_legacy_entry(path: &Path, token: &str) -> Result<(), String> {
    remove_imported_legacy_entry_with(path, token, |path| std::fs::remove_file(path))
}

fn remove_imported_legacy_entry_with(
    path: &Path,
    token: &str,
    remove_file: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    match remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove imported CCR entry {token}: {error}")),
    }
}

fn remove_legacy_entry(path: &Path, token: &str, reason: &str) -> Result<Option<u64>, String> {
    remove_legacy_entry_with(path, token, reason, |path| std::fs::remove_file(path))
}

fn remove_legacy_entry_with(
    path: &Path,
    token: &str,
    reason: &str,
    remove_file: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<Option<u64>, String> {
    let size = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot inspect {reason} legacy CCR entry {token}: {error}"
            ));
        }
    };
    match remove_file(path) {
        Ok(()) => Ok(Some(size)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot remove {reason} legacy CCR entry {token}: {error}"
        )),
    }
}

fn remaining_ttl(modified: SystemTime, now: SystemTime, ttl: Duration) -> Option<Duration> {
    let elapsed = now.duration_since(modified).unwrap_or_default();
    let remaining = ttl.saturating_sub(elapsed);
    (!remaining.is_zero()).then_some(remaining)
}

fn remaining_ttl_from_epoch_ms(
    written_at_ms: u64,
    now: SystemTime,
    ttl: Duration,
) -> Option<Duration> {
    let written_at = std::time::UNIX_EPOCH.checked_add(Duration::from_millis(written_at_ms))?;
    remaining_ttl(written_at, now, ttl)
}

struct CappedCcrStore {
    store: BlobCcrStore,
    blobs: Arc<dyn BlobStore>,
    lock_path: PathBuf,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
}

fn process_lock_path(root: &Path) -> PathBuf {
    root.join("scopes/tinyjuice-cli/.tinyjuice-ccr.lock")
}

fn acquire_process_lock(path: &Path) -> Result<File, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "CCR process lock has no parent directory".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create CCR process-lock directory: {error}"))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("cannot open CCR process lock: {error}"))?;
    lock.lock()
        .map_err(|error| format!("cannot acquire CCR process lock: {error}"))?;
    Ok(lock)
}

impl CcrStore for CappedCcrStore {
    fn put(&self, content: &str) -> CcrPutResult {
        let token = tinyjuice::cache::short_hash(content);
        let _lock = match acquire_process_lock(&self.lock_path) {
            Ok(lock) => lock,
            Err(error) => {
                log::debug!("[tinyjuice_cli] CCR process lock failed: {error}");
                return CcrPutResult::new(token, false);
            }
        };
        match stored_configuration_matches(&self.blobs, self.ttl, self.max_bytes) {
            Ok(true) => {}
            Ok(false) => {
                log::debug!(
                    "[tinyjuice_cli] CCR write skipped because store configuration is stale"
                );
                return CcrPutResult::new(token, false);
            }
            Err(error) => {
                log::debug!("[tinyjuice_cli] CCR configuration check failed: {error}");
                return CcrPutResult::new(token, false);
            }
        }
        if let Err(error) = delete_import_intent(Arc::clone(&self.blobs), &token) {
            log::debug!(
                "[tinyjuice_cli] stale CCR import intent cleanup failed before write: {error}"
            );
            // Do not publish a newer body while stale import chronology could
            // still override its backend write time after an interrupted write.
            return CcrPutResult::new(token, false);
        }
        let result = self.store.put(content);
        if result.retained() {
            if let Err(error) = write_semantic_time(
                Arc::clone(&self.blobs),
                result.token(),
                unix_epoch_ms(SystemTime::now()),
                self.ttl,
            ) {
                log::debug!("[tinyjuice_cli] CCR timestamp write failed: {error}");
                let mut stale_time_cleanup_failed = false;
                if let Err(error) = delete_metadata(Arc::clone(&self.blobs), result.token()) {
                    log::debug!("[tinyjuice_cli] stale CCR timestamp cleanup failed: {error}");
                    stale_time_cleanup_failed = true;
                }
                if let Err(error) = delete_import_intent(Arc::clone(&self.blobs), result.token()) {
                    log::debug!("[tinyjuice_cli] stale CCR import intent cleanup failed: {error}");
                    stale_time_cleanup_failed = true;
                }
                if stale_time_cleanup_failed {
                    return CcrPutResult::new(result.token().to_owned(), false);
                }
            } else if let Err(error) = delete_import_intent(Arc::clone(&self.blobs), result.token())
            {
                log::debug!("[tinyjuice_cli] CCR import intent cleanup failed: {error}");
            }
        }
        if result.retained()
            && (self.ttl.is_some() || self.max_bytes.is_some())
            && let Err(error) = prune(&self.blobs, self.ttl, self.max_bytes, Some(result.token()))
        {
            log::debug!("[tinyjuice_cli] CCR sweep failed: {error}");
        }
        result
    }

    fn get(&self, token: &str) -> Option<String> {
        self.store.get(token)
    }

    fn get_range(
        &self,
        token: &str,
        start: usize,
        end: usize,
        unit: tinyjuice::cache::RangeUnit,
    ) -> Option<String> {
        self.store.get_range(token, start, end, unit)
    }
}

fn list_all(
    blobs: Arc<dyn BlobStore>,
) -> Result<Vec<tinystoragedrivers_types::BlobListEntry>, String> {
    list_all_prefix(blobs, &format!("{CCR_PREFIX}/"))
}

fn list_all_prefix(
    blobs: Arc<dyn BlobStore>,
    prefix: &str,
) -> Result<Vec<tinystoragedrivers_types::BlobListEntry>, String> {
    let mut entries = Vec::new();
    let mut after = None;
    loop {
        let page = run_blob({
            let after = after.clone();
            let blobs = Arc::clone(&blobs);
            let prefix = prefix.to_owned();
            async move { blobs.list_page(&prefix, after.as_deref(), PAGE_SIZE).await }
        })?;
        entries.extend(page.items);
        after = page.next_after;
        if after.is_none() {
            return Ok(entries);
        }
    }
}

async fn list_all_prefix_async(
    blobs: Arc<dyn BlobStore>,
    prefix: &str,
) -> Result<Vec<tinystoragedrivers_types::BlobListEntry>, String> {
    let mut entries = Vec::new();
    let mut after = None;
    loop {
        let page = blobs
            .list_page(prefix, after.as_deref(), PAGE_SIZE)
            .await
            .map_err(|error| error.to_string())?;
        entries.extend(page.items);
        after = page.next_after;
        if after.is_none() {
            return Ok(entries);
        }
    }
}

#[cfg(test)]
fn delete_blob(blobs: Arc<dyn BlobStore>, key: &str) -> Result<(), String> {
    let key = key.to_owned();
    let handle = Arc::clone(&blobs);
    run_blob(async move { handle.delete(&key).await }).map(|_| ())
}

#[cfg(test)]
fn delete_blob_and_metadata(blobs: Arc<dyn BlobStore>, key: &str) -> Result<(), String> {
    delete_blob(Arc::clone(&blobs), key)?;
    let Some(token) = key.strip_prefix(&format!("{CCR_PREFIX}/")) else {
        return Ok(());
    };
    delete_metadata(blobs, token)
}

async fn delete_blob_and_metadata_async(
    blobs: Arc<dyn BlobStore>,
    key: &str,
) -> Result<(), String> {
    blobs.delete(key).await.map_err(|error| error.to_string())?;
    let Some(token) = key.strip_prefix(&format!("{CCR_PREFIX}/")) else {
        return Ok(());
    };
    delete_metadata_async(blobs, token).await
}

fn delete_metadata(blobs: Arc<dyn BlobStore>, token: &str) -> Result<(), String> {
    let key = format!("{CCR_METADATA_PREFIX}{token}");
    let handle = Arc::clone(&blobs);
    run_blob(async move { handle.delete(&key).await }).map(|_| ())
}

async fn delete_metadata_async(blobs: Arc<dyn BlobStore>, token: &str) -> Result<(), String> {
    let key = format!("{CCR_METADATA_PREFIX}{token}");
    blobs
        .delete(&key)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn write_semantic_time(
    blobs: Arc<dyn BlobStore>,
    token: &str,
    written_at_ms: u64,
    ttl: Option<Duration>,
) -> Result<(), String> {
    write_timestamp(blobs, CCR_METADATA_PREFIX, token, written_at_ms, ttl)
}

fn write_import_intent(
    blobs: Arc<dyn BlobStore>,
    token: &str,
    written_at_ms: u64,
    ttl: Option<Duration>,
) -> Result<(), String> {
    write_timestamp(blobs, CCR_IMPORT_PREFIX, token, written_at_ms, ttl)
}

fn read_import_intent(blobs: Arc<dyn BlobStore>, token: &str) -> Result<Option<u64>, String> {
    read_timestamp(blobs, CCR_IMPORT_PREFIX, token)
}

fn delete_import_intent(blobs: Arc<dyn BlobStore>, token: &str) -> Result<(), String> {
    let key = format!("{CCR_IMPORT_PREFIX}{token}");
    run_blob(async move { blobs.delete(&key).await }).map(|_| ())
}

async fn delete_import_intent_async(blobs: Arc<dyn BlobStore>, token: &str) -> Result<(), String> {
    let key = format!("{CCR_IMPORT_PREFIX}{token}");
    blobs
        .delete(&key)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn write_timestamp(
    blobs: Arc<dyn BlobStore>,
    prefix: &str,
    token: &str,
    written_at_ms: u64,
    ttl: Option<Duration>,
) -> Result<(), String> {
    let key = format!("{prefix}{token}");
    let bytes = written_at_ms.to_string().into_bytes();
    run_blob(async move {
        // The port accepts relative TTLs, not absolute expiration timestamps.
        // This sibling write uses the same remaining TTL as the CCR body, with
        // only the small skew between the two backend writes.
        match ttl {
            Some(ttl) => {
                blobs
                    .put_expiring(&key, bytes, Some(CCR_METADATA_CONTENT_TYPE), ttl)
                    .await
            }
            None => {
                blobs
                    .put(&key, bytes, Some(CCR_METADATA_CONTENT_TYPE))
                    .await
            }
        }
    })
    .map(|_| ())
}

fn read_semantic_time(blobs: Arc<dyn BlobStore>, token: &str) -> Result<Option<u64>, String> {
    read_timestamp(blobs, CCR_METADATA_PREFIX, token)
}

fn read_timestamp(
    blobs: Arc<dyn BlobStore>,
    prefix: &str,
    token: &str,
) -> Result<Option<u64>, String> {
    let key = format!("{prefix}{token}");
    let lookup_key = key.clone();
    let Some(blob) = run_blob({
        let blobs = Arc::clone(&blobs);
        async move { blobs.get(&lookup_key).await }
    })?
    else {
        return Ok(None);
    };
    match parse_timestamp(&blob.bytes) {
        Ok(timestamp) => Ok(Some(timestamp)),
        Err(error) => {
            log::warn!("[tinyjuice_cli] discarding malformed CCR timestamp sidecar {key}: {error}");
            run_blob(async move { blobs.delete(&key).await })
                .map(|_| ())
                .map_err(|error| {
                    format!("cannot remove malformed CCR timestamp sidecar: {error}")
                })?;
            Ok(None)
        }
    }
}

async fn read_timestamp_async(
    blobs: Arc<dyn BlobStore>,
    prefix: &str,
    token: &str,
) -> Result<Option<u64>, String> {
    let key = format!("{prefix}{token}");
    let Some(blob) = blobs.get(&key).await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    match parse_timestamp(&blob.bytes) {
        Ok(timestamp) => Ok(Some(timestamp)),
        Err(error) => {
            log::warn!("[tinyjuice_cli] discarding malformed CCR timestamp sidecar {key}: {error}");
            blobs.delete(&key).await.map(|_| ()).map_err(|error| {
                format!("cannot remove malformed CCR timestamp sidecar: {error}")
            })?;
            Ok(None)
        }
    }
}

fn parse_timestamp(bytes: &[u8]) -> Result<u64, String> {
    std::str::from_utf8(bytes)
        .map_err(|error| format!("invalid CCR timestamp metadata: {error}"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid CCR timestamp metadata: {error}"))
}

fn backend_written_at(blobs: Arc<dyn BlobStore>, key: &str) -> Result<Option<u64>, String> {
    let key = key.to_owned();
    let lookup_key = key.clone();
    let page = run_blob(async move { blobs.list_page(&lookup_key, None, 1).await })?;
    Ok(page
        .items
        .into_iter()
        .find(|entry| entry.meta.key == key)
        .and_then(|entry| entry.written_at_ms))
}

#[cfg(test)]
fn list_semantic_times(blobs: Arc<dyn BlobStore>) -> Result<HashMap<String, u64>, String> {
    list_timestamps(blobs, CCR_METADATA_PREFIX)
}

#[cfg(test)]
fn list_timestamps(
    blobs: Arc<dyn BlobStore>,
    prefix: &str,
) -> Result<HashMap<String, u64>, String> {
    let entries = list_all_prefix(Arc::clone(&blobs), prefix)?;
    let mut times = HashMap::new();
    for entry in entries {
        let Some(token) = entry.meta.key.strip_prefix(prefix) else {
            continue;
        };
        if !is_token(token) {
            continue;
        }
        if let Some(timestamp) = read_timestamp(Arc::clone(&blobs), prefix, token)? {
            times.insert(token.to_owned(), timestamp);
        }
    }
    Ok(times)
}

async fn list_timestamps_async(
    blobs: Arc<dyn BlobStore>,
    prefix: &str,
) -> Result<HashMap<String, u64>, String> {
    let entries = list_all_prefix_async(Arc::clone(&blobs), prefix).await?;
    let mut times = HashMap::new();
    for entry in entries {
        let Some(token) = entry.meta.key.strip_prefix(prefix) else {
            continue;
        };
        if !is_token(token) {
            continue;
        }
        if let Some(timestamp) = read_timestamp_async(Arc::clone(&blobs), prefix, token).await? {
            times.insert(token.to_owned(), timestamp);
        }
    }
    Ok(times)
}

fn run_blob<T, E>(
    future: impl std::future::Future<Output = Result<T, E>> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    // Blob engines in this CLI are self-contained and do not depend on work
    // from another runtime. Run the synchronous entry point on one dedicated
    // thread so it remains safe inside a current-thread Tokio runtime.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("cannot start storage runtime: {error}"))?;
        runtime.block_on(future).map_err(|error| error.to_string())
    })
    .join()
    .map_err(|_| "storage worker thread panicked".to_owned())?
}

fn is_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn unix_epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
