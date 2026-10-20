//! Optional filesystem-backed CCR for standalone CLI builds.

use super::store::{DEFAULT_MAX_BYTES, is_valid_token};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, SystemTime};

/// Default total-bytes cap for the CLI disk tier (matches the memory cap).
pub const DEFAULT_DISK_MAX_BYTES: u64 = DEFAULT_MAX_BYTES as u64;
const DISK_CAP_ENFORCE_EVERY: u64 = 128;

fn root() -> &'static RwLock<Option<PathBuf>> {
    static ROOT: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    ROOT.get_or_init(|| RwLock::new(None))
}

fn cap() -> &'static RwLock<Option<u64>> {
    static CAP: OnceLock<RwLock<Option<u64>>> = OnceLock::new();
    CAP.get_or_init(|| RwLock::new(Some(DEFAULT_DISK_MAX_BYTES)))
}

/// Configure the maximum disk-tier bytes enforced during offload.
pub fn configure_disk_cap(max_bytes: Option<u64>) {
    *cap().write().unwrap_or_else(|p| p.into_inner()) = max_bytes;
}

/// Enable the file-backed tier used by standalone CLI commands.
pub fn enable_disk_tier(path: PathBuf) {
    if std::fs::create_dir_all(&path).is_ok() {
        *root().write().unwrap_or_else(|p| p.into_inner()) = Some(path);
    } else {
        log::warn!("[tinyjuice][ccr] could not create CLI disk tier at {path:?}");
    }
}

/// Disable new filesystem writes and lookups for the standalone CLI.
pub fn disable_disk_tier() {
    *root().write().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Disk-tier garbage-collection outcome.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GcStats {
    pub removed: usize,
    pub freed_bytes: u64,
    pub kept: usize,
    pub kept_bytes: u64,
}

/// Store an original in the configured CLI disk tier.
pub(super) fn put(token: &str, content: &str, ttl: Option<Duration>) -> bool {
    let Some(root) = root().read().unwrap_or_else(|p| p.into_inner()).clone() else {
        return false;
    };
    if std::fs::write(root.join(token), content).is_err() {
        return false;
    }
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let n = WRITES.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_multiple_of(DISK_CAP_ENFORCE_EVERY) {
        let max_bytes = *cap().read().unwrap_or_else(|p| p.into_inner());
        if (ttl.is_some() || max_bytes.is_some())
            && let Err(error) = gc_dir_protecting(&root, ttl, max_bytes, Some(token))
        {
            log::debug!("[tinyjuice][ccr] disk-tier sweep failed: {error}");
        }
    }
    true
}

/// Read a token from the CLI disk tier, respecting the configured TTL.
pub(super) fn get(token: &str, ttl: Option<Duration>) -> Option<String> {
    let root = root().read().unwrap_or_else(|p| p.into_inner()).clone()?;
    get_from(&root, token, ttl)
}

fn get_from(root: &Path, token: &str, ttl: Option<Duration>) -> Option<String> {
    let path = root.join(token);
    if disk_entry_expired(&path, ttl) {
        log::debug!("[tinyjuice][ccr] disk entry expired for {token}");
        let _ = std::fs::remove_file(path);
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Sweep the configured CLI disk tier.
pub fn gc_disk_tier(ttl: Option<Duration>, max_bytes: Option<u64>) -> io::Result<GcStats> {
    gc_disk_tier_at(
        root().read().unwrap_or_else(|p| p.into_inner()).clone(),
        ttl,
        max_bytes,
    )
}

fn gc_disk_tier_at(
    path: Option<PathBuf>,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
) -> io::Result<GcStats> {
    let Some(path) = path else {
        return Ok(GcStats::default());
    };
    gc_disk_dir(&path, ttl, max_bytes)
}

/// Sweep one explicit directory, considering files named by valid CCR tokens.
pub fn gc_disk_dir(
    path: &Path,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
) -> io::Result<GcStats> {
    gc_dir_protecting(path, ttl, max_bytes, None)
}

fn gc_dir_protecting(
    root: &Path,
    ttl: Option<Duration>,
    max_bytes: Option<u64>,
    protect: Option<&str>,
) -> io::Result<GcStats> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(GcStats::default()),
        Err(error) => return Err(error),
    };
    let mut stats = GcStats::default();
    let mut kept: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let now = SystemTime::now();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_valid_token(name) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let bytes = metadata.len();
        let modified = metadata.modified().unwrap_or(now);
        let expired = ttl.is_some_and(|ttl| {
            now.duration_since(modified)
                .is_ok_and(|elapsed| elapsed >= ttl)
        });
        if expired && std::fs::remove_file(entry.path()).is_ok() {
            stats.removed += 1;
            stats.freed_bytes += bytes;
        } else {
            kept.push((entry.path(), bytes, modified));
        }
    }
    let mut total: u64 = kept.iter().map(|(_, bytes, _)| bytes).sum();
    if let Some(max_bytes) = max_bytes {
        kept.sort_by_key(|(_, _, modified)| *modified);
        let mut index = 0;
        while total > max_bytes && index < kept.len() {
            let (path, bytes, _) = &kept[index];
            let protected =
                protect.is_some_and(|token| path.file_name().is_some_and(|name| name == token));
            if !protected && std::fs::remove_file(path).is_ok() {
                stats.removed += 1;
                stats.freed_bytes += bytes;
                total -= bytes;
                kept.remove(index);
            } else {
                index += 1;
            }
        }
    }
    stats.kept = kept.len();
    stats.kept_bytes = total;
    Ok(stats)
}

fn disk_entry_expired(path: &Path, ttl: Option<Duration>) -> bool {
    let Some(ttl) = ttl else { return false };
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified.elapsed().is_ok_and(|elapsed| elapsed >= ttl)
}

#[cfg(test)]
#[path = "disk_tier_tests.rs"]
mod tests;
