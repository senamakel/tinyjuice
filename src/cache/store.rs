//! CCR — Compress-Cache-Retrieve store.
//!
//! When a compressor drops data (lossy paths), the router stows the original
//! here keyed by a short content hash and embeds a retrieval marker in the
//! compacted text (see [`super::marker`]). The agent calls the
//! `juice_retrieve` tool to get the original back on demand — so even
//! aggressive compaction stays reversible and is safe under the always-on
//! default.
//!
//! Process-global and bounded by **both** an entry count and a total byte cap,
//! with an optional TTL. Keyed by content hash, so re-offloading identical
//! content is idempotent. Persistence is supplied through [`CcrStore`].

use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// Default max originals retained (entry-count cap).
pub const DEFAULT_MAX_ENTRIES: usize = 256;
/// Default total-bytes cap (64 MiB) so a few huge originals can't blow memory.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Bytes of the SHA-256 digest used for the key (→ 32 hex chars). Wide enough
/// that collisions are infeasible and the hash doubles as an unguessable
/// capability token.
const HASH_BYTES: usize = 16;

/// Result of attempting to store an original in CCR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcrPutResult {
    token: String,
    retained: bool,
}

impl CcrPutResult {
    /// Build a put result. Invalid token shapes are never reported as retained.
    pub fn new(token: String, retained: bool) -> Self {
        let retained = retained && is_valid_token(&token);
        Self { token, retained }
    }

    /// CCR token derived for the original.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// True when the store retained the original and retrieval may be advertised.
    pub fn retained(&self) -> bool {
        self.retained
    }
}

/// Injectable CCR store.
pub trait CcrStore: Send + Sync {
    fn put(&self, content: &str) -> CcrPutResult;
    fn get(&self, token: &str) -> Option<String>;
    fn get_range(&self, token: &str, start: usize, end: usize, unit: RangeUnit) -> Option<String> {
        let original = self.get(token)?;
        range_from_original(&original, start, end, unit)
    }
}

/// Compatibility wrapper over the process-global CCR store.
#[derive(Debug, Default, Clone, Copy)]
pub struct GlobalCcrStore;

impl CcrStore for GlobalCcrStore {
    fn put(&self, content: &str) -> CcrPutResult {
        let (token, retained) = offload_checked(content);
        CcrPutResult::new(token, retained)
    }

    fn get(&self, token: &str) -> Option<String> {
        retrieve(token)
    }

    fn get_range(&self, token: &str, start: usize, end: usize, unit: RangeUnit) -> Option<String> {
        retrieve_range(token, start, end, unit)
    }
}

/// Isolated in-memory CCR store for tests and host-managed adapters.
pub struct MemoryCcrStore {
    inner: Mutex<Inner>,
    max_entries: usize,
    max_bytes: usize,
    ttl: Option<Duration>,
}

impl Default for MemoryCcrStore {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ENTRIES, DEFAULT_MAX_BYTES)
    }
}

impl MemoryCcrStore {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1),
            ttl: None,
        }
    }

    pub fn with_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn stats(&self) -> (usize, usize) {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        (inner.map.len(), inner.total_bytes)
    }
}

impl CcrStore for MemoryCcrStore {
    fn put(&self, content: &str) -> CcrPutResult {
        let token = short_hash(content);
        let mem_retained = self.inner.lock().unwrap_or_else(|p| p.into_inner()).insert(
            token.clone(),
            content.to_string(),
            self.max_entries,
            self.max_bytes,
        );
        CcrPutResult::new(token, mem_retained)
    }

    fn get(&self, token: &str) -> Option<String> {
        if !is_valid_token(token) {
            return None;
        }
        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = inner.map.get(token) {
                if self.ttl.is_none_or(|ttl| entry.created.elapsed() < ttl) {
                    return Some(entry.content.clone());
                }
                if let Some(entry) = inner.map.remove(token) {
                    inner.total_bytes = inner.total_bytes.saturating_sub(entry.content.len());
                }
            }
        }
        None
    }
}

/// Tunable limits, settable once at startup from the `[tinyjuice]` config.
struct Limits {
    max_entries: usize,
    max_bytes: usize,
    ttl: Option<Duration>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_ENTRIES,
            max_bytes: DEFAULT_MAX_BYTES,
            ttl: None,
        }
    }
}

fn limits() -> &'static RwLock<Limits> {
    static LIMITS: OnceLock<RwLock<Limits>> = OnceLock::new();
    LIMITS.get_or_init(|| RwLock::new(Limits::default()))
}

/// Configure the cache limits (called once from config at startup).
pub fn configure(max_entries: usize, max_bytes: usize, ttl_secs: Option<u64>) {
    let mut l = limits().write().unwrap_or_else(|p| p.into_inner());
    l.max_entries = max_entries.max(1);
    l.max_bytes = max_bytes.max(1);
    l.ttl = ttl_secs.map(Duration::from_secs);
}

/// The host-provided durable tier, tried after memory. See [`set_durable_tier`].
fn durable_tier() -> &'static RwLock<Option<Arc<dyn CcrStore>>> {
    static TIER: OnceLock<RwLock<Option<Arc<dyn CcrStore>>>> = OnceLock::new();
    TIER.get_or_init(|| RwLock::new(None))
}

/// Install (or clear, with `None`) a durable tier behind the in-memory cache.
///
/// The store owns persistence and expiry, so this crate stays free of any
/// storage dependency: a host module hands it a [`CcrStore`] built on whatever
/// backend it was given. Offloads mirror into it and a memory miss falls
/// through to it. The CLI's optional file tier is separate from this store.
pub fn set_durable_tier(store: Option<Arc<dyn CcrStore>>) {
    *durable_tier().write().unwrap_or_else(|p| p.into_inner()) = store;
}

struct Entry {
    content: String,
    created: Instant,
}

#[derive(Default)]
struct Inner {
    map: HashMap<String, Entry>,
    order: VecDeque<String>,
    total_bytes: usize,
}

impl Inner {
    /// Insert `content` under `hash` (idempotent) and evict (FIFO) until both
    /// the entry-count and total-byte caps hold.
    ///
    /// Returns whether `hash` is still resident after eviction. A single
    /// original larger than `max_bytes` cannot be retained in memory under the
    /// byte cap — eviction would immediately drop the just-inserted entry — so
    /// `false` is returned and the caller must not advertise it as recoverable
    /// (the router declines lossy compaction unless a durable store retains it).
    fn insert(
        &mut self,
        hash: String,
        content: String,
        max_entries: usize,
        max_bytes: usize,
    ) -> bool {
        if let Some(entry) = self.map.get_mut(&hash) {
            entry.created = Instant::now();
            self.order.retain(|candidate| candidate != &hash);
            self.order.push_back(hash);
            return true;
        }
        let bytes = content.len();
        self.total_bytes += bytes;
        self.map.insert(
            hash.clone(),
            Entry {
                content,
                created: Instant::now(),
            },
        );
        self.order.push_back(hash.clone());
        while self.order.len() > max_entries || self.total_bytes > max_bytes {
            // Never evict the entry we just inserted to satisfy the cap when it
            // is the only thing keeping us over: that would make the original
            // unrecoverable the instant its footer is emitted. Stop and report
            // non-retention instead (one oversized item is rejected, not the
            // whole store wiped).
            if self.order.len() == 1 {
                break;
            }
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if let Some(e) = self.map.remove(&evicted) {
                self.total_bytes = self.total_bytes.saturating_sub(e.content.len());
            }
        }
        // Retained iff still present (an oversized single entry over the byte
        // cap is dropped below) AND within the byte cap.
        if self.total_bytes > max_bytes {
            if let Some(e) = self.map.remove(&hash) {
                self.total_bytes = self.total_bytes.saturating_sub(e.content.len());
            }
            self.order.retain(|h| h != &hash);
            return false;
        }
        self.map.contains_key(&hash)
    }
}

fn global() -> &'static Mutex<Inner> {
    static STORE: OnceLock<Mutex<Inner>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Inner::default()))
}

/// Stash `content` and return its short hash. Idempotent for identical content.
pub fn offload(content: &str) -> String {
    offload_checked(content).0
}

/// Stash `content`, returning `(hash, retained)`. `retained` is `false` only
/// when the original could be kept neither in memory (it exceeds the byte cap)
/// nor in a durable host store — in which case the caller must NOT advertise it as
/// recoverable. Idempotent for identical content.
pub fn offload_checked(content: &str) -> (String, bool) {
    let hash = short_hash(content);
    let retained = offload_checked_with_hash(&hash, content);
    (hash, retained)
}

/// Like [`offload_checked`] but takes the precomputed `short_hash(content)`
/// so callers that already have it (e.g. the router builds the footer from the
/// hash before deciding to store) don't pay for a second SHA-256 pass.
/// Returns whether the original was retained (memory or the host's durable store).
pub fn offload_checked_with_hash(hash: &str, content: &str) -> bool {
    debug_assert_eq!(hash, short_hash(content), "hash must match content");
    let (max_entries, max_bytes) = {
        let l = limits().read().unwrap_or_else(|p| p.into_inner());
        (l.max_entries, l.max_bytes)
    };
    let mem_retained = global().lock().unwrap_or_else(|p| p.into_inner()).insert(
        hash.to_string(),
        content.to_string(),
        max_entries,
        max_bytes,
    );

    // Mirror to the host-provided durable tier (best-effort).
    let durable = durable_tier()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    // Retention counts only under the recovery token (`hash`) callers advertise:
    // a store that keys the original differently cannot serve `retrieve(hash)`.
    let durable_retained = durable.is_some_and(|store| {
        let put = store.put(content);
        let same_token = put.token() == hash;
        if put.retained() && !same_token {
            log::debug!(
                "[tinyjuice][ccr] durable tier keyed {hash} under another token; not counted"
            );
        }
        put.retained() && same_token
    });
    mem_retained || durable_retained
}

/// True if `hash` is a well-formed CCR token (exactly the generated hex digest).
/// Tokens come from agent-controlled tool args. Reject anything other than the
/// fixed hex shape (e.g. `../../state/config.toml`) before looking up any store.
pub(super) fn is_valid_token(hash: &str) -> bool {
    hash.len() == HASH_BYTES * 2 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Retrieve a previously-offloaded original by hash, if still available
/// (memory first, then a host durable store).
/// Honours the in-memory TTL; durable stores own their own expiry.
pub fn retrieve(hash: &str) -> Option<String> {
    // Reject anything that isn't the generated token shape up front.
    if !is_valid_token(hash) {
        return None;
    }
    let ttl = limits().read().unwrap_or_else(|p| p.into_inner()).ttl;
    {
        let mut inner = global().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = inner.map.get(hash) {
            if ttl.is_none_or(|t| entry.created.elapsed() < t) {
                return Some(entry.content.clone());
            }
            // Expired — drop it and fall through to durable storage.
            if let Some(e) = inner.map.remove(hash) {
                inner.total_bytes = inner.total_bytes.saturating_sub(e.content.len());
            }
        }
    }
    // Host-provided durable tier: it owns its own expiry.
    let durable = durable_tier()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if let Some(found) = durable.and_then(|store| store.get(hash)) {
        return Some(found);
    }
    None
}

/// The span/unit for a ranged retrieval.
#[derive(Debug, Clone, Copy)]
pub enum RangeUnit {
    Bytes,
    Lines,
}

/// Retrieve a slice of a previously-offloaded original. `start`/`end` are
/// 0-based, `end` exclusive; out-of-bounds ends are clamped. Returns `None`
/// only when the original isn't available at all.
pub fn retrieve_range(hash: &str, start: usize, end: usize, unit: RangeUnit) -> Option<String> {
    let original = retrieve(hash)?;
    range_from_original(&original, start, end, unit)
}

/// Slice `original` as [`retrieve_range`] does. Public so a [`CcrStore`] backed
/// by another tier can share the clamping rules.
pub fn range_from_original(
    original: &str,
    start: usize,
    end: usize,
    unit: RangeUnit,
) -> Option<String> {
    if end <= start {
        return Some(String::new());
    }
    match unit {
        RangeUnit::Bytes => {
            // Clamp to char boundaries so we never split a UTF-8 sequence.
            let s = floor_char_boundary(original, start.min(original.len()));
            let e = floor_char_boundary(original, end.min(original.len()));
            Some(original[s..e].to_string())
        }
        RangeUnit::Lines => {
            let lines: Vec<&str> = original.lines().collect();
            let e = end.min(lines.len());
            if start >= lines.len() {
                return Some(String::new());
            }
            Some(lines[start..e].join("\n"))
        }
    }
}

/// Largest char boundary ≤ `idx` (std's `floor_char_boundary` is still nightly).
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Short hex content hash used as the CCR key/token.
pub fn short_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..HASH_BYTES])
}

/// Snapshot of cache occupancy for the debug controller / stats.
pub fn stats() -> (usize, usize) {
    let inner = global().lock().unwrap_or_else(|p| p.into_inner());
    (inner.map.len(), inner.total_bytes)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
