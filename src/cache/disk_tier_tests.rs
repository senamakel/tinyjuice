use super::*;
use crate::cache::{CcrStore, GlobalCcrStore, RangeUnit, disable_disk_tier, short_hash, stats};

/// Write a token-named file with a back-dated mtime.
fn write_aged(dir: &Path, name: &str, bytes: usize, age: Duration) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "x".repeat(bytes)).unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(SystemTime::now() - age).unwrap();
    path
}

fn token(fill: char) -> String {
    fill.to_string().repeat(32)
}

#[test]
fn gc_removes_expired_keeps_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let old = write_aged(dir.path(), &token('a'), 10, Duration::from_secs(600));
    let fresh = write_aged(dir.path(), &token('b'), 20, Duration::ZERO);

    let stats = gc_disk_dir(dir.path(), Some(Duration::from_secs(60)), None).unwrap();
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.freed_bytes, 10);
    assert_eq!(stats.kept, 1);
    assert_eq!(stats.kept_bytes, 20);
    assert!(!old.exists());
    assert!(fresh.exists());
}

#[test]
fn gc_evicts_oldest_over_budget() {
    let dir = tempfile::tempdir().unwrap();
    let oldest = write_aged(dir.path(), &token('a'), 100, Duration::from_secs(30));
    let middle = write_aged(dir.path(), &token('b'), 100, Duration::from_secs(20));
    let newest = write_aged(dir.path(), &token('c'), 100, Duration::from_secs(10));

    let stats = gc_disk_dir(dir.path(), None, Some(250)).unwrap();
    assert_eq!(stats.removed, 1, "one eviction brings 300 under 250");
    assert_eq!(stats.freed_bytes, 100);
    assert_eq!(stats.kept, 2);
    assert!(!oldest.exists(), "oldest-by-mtime goes first");
    assert!(middle.exists());
    assert!(newest.exists());
}

#[test]
fn gc_ignores_non_token_files_and_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    let stray = dir.path().join("README.txt");
    std::fs::write(&stray, "not a ccr entry").unwrap();

    let stats = gc_disk_dir(dir.path(), Some(Duration::ZERO), Some(0)).unwrap();
    assert_eq!(stats, GcStats::default());
    assert!(stray.exists(), "non-token files are never touched");

    let missing = dir.path().join("does-not-exist");
    assert_eq!(
        gc_disk_dir(&missing, None, None).unwrap(),
        GcStats::default()
    );
}

#[test]
fn gc_protects_named_entry_from_budget_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let protected_name = token('a');
    let protected = write_aged(dir.path(), &protected_name, 100, Duration::from_secs(30));
    let newer = write_aged(dir.path(), &token('b'), 100, Duration::from_secs(10));

    let stats = gc_dir_protecting(dir.path(), None, Some(150), Some(&protected_name)).unwrap();
    assert!(
        protected.exists(),
        "protected entry survives even when oldest"
    );
    assert!(!newer.exists(), "eviction falls through to the next-oldest");
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.kept, 1);
}

#[test]
fn disk_entry_ttl_uses_file_mtime() {
    let dir = std::env::temp_dir().join(format!("tj-ccr-ttl-{}", short_hash("disk-ttl")));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    std::fs::write(&path, "disk ttl payload").unwrap();

    assert!(!disk_entry_expired(&path, None));
    assert!(!disk_entry_expired(&path, Some(Duration::from_secs(60))));
    assert!(disk_entry_expired(&path, Some(Duration::ZERO)));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn configured_tier_reads_entries_expires_them_and_runs_gc() {
    let dir = tempfile::tempdir().unwrap();
    let name = token('d');
    let path = dir.path().join(&name);
    std::fs::write(&path, "disk-backed payload").unwrap();

    assert_eq!(
        get_from(dir.path(), &name, None).as_deref(),
        Some("disk-backed payload")
    );
    let sweep = gc_disk_dir(dir.path(), None, None).unwrap();
    assert_eq!(sweep.kept, 1);

    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        get_from(dir.path(), &name, Some(Duration::from_secs(1))),
        None
    );
    assert!(!path.exists(), "expired reads remove the disk entry");
}

#[test]
fn configured_gc_sweeps_the_selected_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(token('a'));
    std::fs::write(&path, "remove me").unwrap();

    let stats = gc_disk_tier_at(Some(dir.path().to_path_buf()), None, Some(0)).unwrap();
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.freed_bytes, 9);
    assert!(!path.exists());
    assert_eq!(
        gc_disk_tier_at(None, None, None).unwrap(),
        GcStats::default()
    );
}

#[test]
fn enabling_tier_with_a_file_as_root_is_best_effort() {
    let _guard = crate::cache::DISK_TIER_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, "keep this file").unwrap();

    enable_disk_tier(file.clone());

    assert_eq!(std::fs::read_to_string(file).unwrap(), "keep this file");
    let memory = crate::cache::MemoryCcrStore::new(4, 1024);
    let stored = memory.put("memory remains available");
    assert!(stored.retained());
    assert_eq!(
        memory.get(stored.token()).as_deref(),
        Some("memory remains available")
    );
    disable_disk_tier();
}

#[test]
fn periodic_write_sweep_removes_expired_entries() {
    let _guard = crate::cache::DISK_TIER_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    enable_disk_tier(dir.path().to_path_buf());
    const TTL_SECS: u64 = 30 * 24 * 60 * 60;
    crate::cache::configure(usize::MAX, usize::MAX, Some(TTL_SECS));
    configure_disk_cap(None);

    let expired = write_aged(
        dir.path(),
        &token('a'),
        10,
        Duration::from_secs(TTL_SECS + 60),
    );
    // A full interval guarantees that the process-wide write counter reaches a
    // sweep regardless of where other tests left it.
    for index in 0..DISK_CAP_ENFORCE_EVERY {
        let content = format!("fresh original {index}");
        let (_, retained) = crate::cache::offload_checked(&content);
        assert!(retained, "fixture write {index} must succeed");
    }

    let expired_remains = expired.exists();
    disable_disk_tier();
    crate::cache::configure(
        crate::cache::DEFAULT_MAX_ENTRIES,
        crate::cache::DEFAULT_MAX_BYTES,
        None,
    );
    configure_disk_cap(Some(DEFAULT_DISK_MAX_BYTES));
    assert!(
        !expired_remains,
        "periodic sweep must enforce the configured TTL"
    );
}

#[test]
fn garbage_collection_skips_nonentries_and_protects_inflight_original() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes"), "unrelated").unwrap();
    std::fs::create_dir(dir.path().join("a".repeat(32))).unwrap();
    let protected = short_hash("protected");
    std::fs::write(dir.path().join(&protected), "protected").unwrap();
    let removed = short_hash("removed");
    std::fs::write(dir.path().join(&removed), "removed").unwrap();
    let stats = gc_dir_protecting(dir.path(), None, Some(0), Some(&protected)).unwrap();
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.kept, 1);
    assert_eq!(stats.kept_bytes, 9);
    assert!(dir.path().join(&protected).exists());
    assert!(dir.path().join("notes").exists());
    assert_eq!(
        gc_disk_dir(&dir.path().join("absent"), None, None).unwrap(),
        GcStats::default()
    );
    assert!(gc_disk_dir(&dir.path().join("notes"), None, None).is_err());
}

#[test]
fn global_adapter_supports_ranges_and_unconfigured_disk_gc() {
    let _guard = crate::cache::DISK_TIER_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    disable_disk_tier();
    assert!(gc_disk_tier(None, None).is_ok());
    assert!(!disk_entry_expired(
        &PathBuf::from("/nonexistent-tinyjuice-fixture"),
        Some(Duration::ZERO)
    ));
}
