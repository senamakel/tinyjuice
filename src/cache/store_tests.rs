use super::*;

#[test]
fn round_trips() {
    let original = "ccr round-trip unique payload alpha ".repeat(50);
    let hash = offload(&original);
    assert_eq!(hash.len(), HASH_BYTES * 2);
    assert_eq!(retrieve(&hash).as_deref(), Some(original.as_str()));
}

#[test]
fn idempotent_hash() {
    let a = offload("ccr idempotent unique payload bravo content");
    let b = offload("ccr idempotent unique payload bravo content");
    assert_eq!(a, b);
}

#[test]
fn missing_hash_is_none() {
    assert!(retrieve("ffffffffffffffffffffffffffffffff").is_none());
}

#[test]
fn byte_cap_evicts_oldest() {
    let mut inner = Inner::default();
    // 10 entries of 100 bytes; cap at 500 bytes ⇒ keep ~5 newest.
    for i in 0..10 {
        inner.insert(format!("h{i}"), "x".repeat(100), 1000, 500);
    }
    assert!(
        inner.total_bytes <= 500,
        "byte cap held: {}",
        inner.total_bytes
    );
    assert!(!inner.map.contains_key("h0"), "oldest evicted");
    assert!(inner.map.contains_key("h9"), "newest retained");
}

#[test]
fn oversized_single_entry_is_not_retained() {
    // One entry larger than the byte cap can't be kept; insert reports
    // non-retention so the router won't advertise it as recoverable.
    let mut inner = Inner::default();
    let retained = inner.insert("big".into(), "x".repeat(200), 100, 100);
    assert!(!retained, "oversized single entry must report not-retained");
    assert!(!inner.map.contains_key("big"));
    assert_eq!(inner.total_bytes, 0);
}

#[test]
fn rejects_path_traversal_tokens() {
    // Non-hex / wrong-length tokens are rejected before any disk join.
    assert!(!is_valid_token("../../state/config.toml"));
    assert!(!is_valid_token("..%2f..%2fetc"));
    assert!(!is_valid_token("deadbeef")); // too short
    assert!(!is_valid_token(&"g".repeat(32))); // non-hex
    assert!(is_valid_token(&"a1b2c3d4".repeat(4))); // 32 hex chars
    // retrieve() returns None for an invalid token regardless of cache state.
    assert!(retrieve("../../state/config.toml").is_none());
}

#[test]
fn within_cap_entry_is_retained() {
    let mut inner = Inner::default();
    assert!(inner.insert("ok".into(), "x".repeat(50), 100, 100));
    assert!(inner.map.contains_key("ok"));
}

#[test]
fn entry_cap_evicts_oldest() {
    let mut inner = Inner::default();
    for i in 0..60 {
        inner.insert(format!("e{i}"), format!("content-{i}"), 50, usize::MAX);
    }
    assert!(inner.map.len() <= 50);
    assert!(!inner.map.contains_key("e0"));
}

#[test]
fn reoffloading_existing_entry_refreshes_ttl_and_order() {
    let mut inner = Inner::default();
    assert!(inner.insert("old".into(), "old payload".into(), 2, usize::MAX));
    assert!(inner.insert("fresh".into(), "fresh payload".into(), 2, usize::MAX));
    let stale_created = Instant::now() - Duration::from_secs(60);
    inner.map.get_mut("old").unwrap().created = stale_created;

    assert!(inner.insert("old".into(), "old payload".into(), 2, usize::MAX));
    assert!(
        inner.map["old"].created > stale_created,
        "existing entry timestamp should refresh"
    );
    assert_eq!(inner.order.back().map(String::as_str), Some("old"));

    assert!(inner.insert("third".into(), "third payload".into(), 2, usize::MAX));
    assert!(inner.map.contains_key("old"));
    assert!(!inner.map.contains_key("fresh"));
}

#[test]
fn range_retrieval_lines_and_bytes() {
    let original = "line0\nline1\nline2\nline3\nline4";
    let hash = offload(original);
    assert_eq!(
        retrieve_range(&hash, 1, 3, RangeUnit::Lines).as_deref(),
        Some("line1\nline2")
    );
    assert_eq!(
        retrieve_range(&hash, 0, 5, RangeUnit::Bytes).as_deref(),
        Some("line0")
    );
    // Out-of-bounds end clamps.
    assert_eq!(
        retrieve_range(&hash, 4, 999, RangeUnit::Lines).as_deref(),
        Some("line4")
    );
}

#[test]
fn memory_store_is_isolated_from_global_cache() {
    let store = MemoryCcrStore::new(10, 10_000);
    let original = "isolated memory store payload delta ".repeat(20);
    let put = store.put(&original);

    assert!(put.retained());
    assert_eq!(store.get(put.token()).as_deref(), Some(original.as_str()));
    assert_eq!(retrieve(put.token()), None, "global cache must not see it");
}

#[test]
fn memory_store_rejects_oversized_originals() {
    let store = MemoryCcrStore::new(10, 16);
    let original = "oversized isolated payload echo ".repeat(20);
    let put = store.put(&original);

    assert!(!put.retained());
    assert_eq!(store.get(put.token()), None);
    assert_eq!(store.stats(), (0, 0));
}

#[test]
fn an_oversized_original_does_not_evict_the_retained_ones() {
    let mut inner = Inner::default();
    inner.insert("a".into(), "x".repeat(40), 100, 100);
    inner.insert("b".into(), "y".repeat(40), 100, 100);
    assert!(!inner.insert("big".into(), "z".repeat(200), 100, 100));
    assert!(inner.map.contains_key("a") && inner.map.contains_key("b"));
    assert_eq!(inner.total_bytes, 80);

    let store = MemoryCcrStore::new(10, 64);
    let kept = store.put("kept isolated payload foxtrot");
    assert!(!store.put(&"oversized payload golf ".repeat(20)).retained());
    assert_eq!(
        store.get(kept.token()).as_deref(),
        Some("kept isolated payload foxtrot")
    );
}

#[test]
fn the_entry_cap_never_evicts_the_original_just_inserted() {
    let mut inner = Inner::default();
    assert!(inner.insert("only".into(), "x".repeat(10), 0, 100));
    assert!(inner.map.contains_key("only"));
}

#[test]
fn memory_store_rejects_malformed_tokens() {
    let store = MemoryCcrStore::new(10, 10_000);

    assert_eq!(store.get("../../state/config.toml"), None);
    assert_eq!(
        store.get_range("../../state/config.toml", 0, 1, RangeUnit::Lines),
        None
    );
}

#[test]
fn offload_with_hash_round_trips() {
    let original = "ccr precomputed-hash unique payload delta ".repeat(30);
    let hash = short_hash(&original);
    assert!(offload_checked_with_hash(&hash, &original));
    assert_eq!(retrieve(&hash).as_deref(), Some(original.as_str()));
    // Matches the hashing path exactly.
    assert_eq!(offload_checked(&original).0, hash);
}

#[test]
fn default_range_retrieval_clamps_utf8_and_empty_ranges() {
    let store = MemoryCcrStore::default();
    let put = store.put("é\nsecond");
    assert_eq!(
        store
            .get_range(put.token(), 1, 2, RangeUnit::Bytes)
            .as_deref(),
        Some("é")
    );
    assert_eq!(
        store
            .get_range(put.token(), 1, 99, RangeUnit::Lines)
            .as_deref(),
        Some("second")
    );
    assert_eq!(
        store
            .get_range(put.token(), 8, 9, RangeUnit::Lines)
            .as_deref(),
        Some("")
    );
    assert_eq!(
        store
            .get_range(put.token(), 2, 1, RangeUnit::Bytes)
            .as_deref(),
        Some("")
    );
}

#[test]
fn global_adapter_supports_ranges_and_unconfigured_disk_gc() {
    let store = GlobalCcrStore;
    let put = store.put("global ranged retrieval fixture\nsecond");
    assert!(put.retained());
    assert_eq!(
        store
            .get_range(put.token(), 1, 2, RangeUnit::Lines)
            .as_deref(),
        Some("second")
    );
    assert!(stats().0 > 0);
}

#[test]
fn isolated_store_recovers_its_contents_after_lock_poisoning() {
    let store = MemoryCcrStore::default();
    let put = store.put("poison recovery fixture");
    let _ = std::panic::catch_unwind(|| {
        let _lock = store.inner.lock().unwrap();
        panic!("intentional isolated-store lock poison");
    });
    assert_eq!(
        store.get(put.token()).as_deref(),
        Some("poison recovery fixture")
    );
    assert_eq!(store.stats(), (1, 23));
    let next = store.put("second fixture");
    assert!(next.retained());
}

/// The durable tier is process-global; tests that install one take turns.
static DURABLE_TIER: Mutex<()> = Mutex::new(());

#[test]
fn durable_tier_mirrors_offloads_and_serves_memory_misses() {
    let _turn = DURABLE_TIER.lock().unwrap_or_else(|p| p.into_inner());
    let durable = Arc::new(MemoryCcrStore::default());
    set_durable_tier(Some(durable.clone()));

    let mirrored = "durable tier mirrored offload fixture";
    let (hash, retained) = offload_checked(mirrored);
    assert!(retained);
    assert_eq!(durable.get(&hash).as_deref(), Some(mirrored));

    // An original only the durable tier holds (evicted or from before a
    // restart) is still retrievable, whole and by range.
    let only_durable = durable.put("durable only\nsecond line");
    assert_eq!(
        retrieve(only_durable.token()).as_deref(),
        Some("durable only\nsecond line")
    );
    assert_eq!(
        retrieve_range(only_durable.token(), 1, 2, RangeUnit::Lines).as_deref(),
        Some("second line")
    );

    set_durable_tier(None);
    assert_eq!(retrieve(only_durable.token()), None, "cleared tier is gone");
}

/// Retains every original, but under a token of its own rather than the
/// content hash callers advertise.
struct OtherToken;

impl CcrStore for OtherToken {
    fn put(&self, _content: &str) -> CcrPutResult {
        CcrPutResult::new(short_hash("another token"), true)
    }
    fn get(&self, _token: &str) -> Option<String> {
        None
    }
    fn get_range(&self, _: &str, _: usize, _: usize, _: RangeUnit) -> Option<String> {
        None
    }
}

#[test]
fn durable_retention_requires_the_recovery_token() {
    let _turn = DURABLE_TIER.lock().unwrap_or_else(|p| p.into_inner());
    set_durable_tier(Some(Arc::new(OtherToken)));
    // Larger than the memory cap, so only the durable tier could retain it.
    let oversized = "x".repeat(DEFAULT_MAX_BYTES + 1);
    let (_hash, retained) = offload_checked(&oversized);
    set_durable_tier(None);
    assert!(
        !retained,
        "a store keyed by another token cannot serve the hash"
    );
}
