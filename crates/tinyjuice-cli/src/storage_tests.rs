use super::*;
use tinyjuice::cache::{CcrStore, RangeUnit};
use tinystoragedrivers_api::BlobStore;
use tinystoragedrivers_testkit::FaultPlan;
use tinystoragedrivers_types::{
    Blob, BlobListPage, BlobMeta, Capabilities, ErrorKind, Result as StorageResult, StorageError,
};

#[derive(Debug)]
struct FailMetadataOperations {
    inner: Arc<dyn BlobStore>,
    faults: FaultPlan,
}

#[async_trait::async_trait]
impl BlobStore for FailMetadataOperations {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn max_key_len(&self) -> usize {
        self.inner.max_key_len()
    }

    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: Option<&str>,
    ) -> StorageResult<BlobMeta> {
        let fail_after_body_write = key.starts_with(&format!("{CCR_PREFIX}/"))
            && self.faults.check("body_put_after_commit").is_err();
        if key.starts_with(CCR_METADATA_PREFIX) {
            self.faults.check("metadata_put")?;
        }
        let metadata = self.inner.put(key, bytes, content_type).await?;
        if fail_after_body_write {
            Err(StorageError::new(
                ErrorKind::Backend,
                "simulated lost body-write acknowledgement",
            ))
        } else {
            Ok(metadata)
        }
    }

    async fn get(&self, key: &str) -> StorageResult<Option<Blob>> {
        if key.starts_with(CCR_METADATA_PREFIX) || key.starts_with(CCR_IMPORT_PREFIX) {
            self.faults.check("metadata_get")?;
        }
        self.inner.get(key).await
    }

    async fn get_range(
        &self,
        key: &str,
        range: std::ops::Range<u64>,
    ) -> StorageResult<Option<Vec<u8>>> {
        self.inner.get_range(key, range).await
    }

    async fn head(&self, key: &str) -> StorageResult<Option<BlobMeta>> {
        self.inner.head(key).await
    }

    async fn delete(&self, key: &str) -> StorageResult<bool> {
        if key.starts_with(CCR_METADATA_PREFIX) {
            self.faults.check("semantic_metadata_delete")?;
        }
        self.inner.delete(key).await
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<BlobMeta>> {
        self.inner.list(prefix).await
    }

    async fn list_page(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> StorageResult<BlobListPage> {
        self.inner.list_page(prefix, after, limit).await
    }
}

#[derive(Debug)]
struct SynchronizeConcurrentPuts {
    inner: Arc<dyn BlobStore>,
    body_writes: Arc<tokio::sync::Barrier>,
    prune_reads: Arc<tokio::sync::Barrier>,
}

#[async_trait::async_trait]
impl BlobStore for SynchronizeConcurrentPuts {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn max_key_len(&self) -> usize {
        self.inner.max_key_len()
    }

    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: Option<&str>,
    ) -> StorageResult<BlobMeta> {
        let metadata = self.inner.put(key, bytes, content_type).await?;
        if key.starts_with(&format!("{CCR_PREFIX}/")) {
            let _ = tokio::time::timeout(Duration::from_millis(500), self.body_writes.wait()).await;
        }
        Ok(metadata)
    }

    async fn get(&self, key: &str) -> StorageResult<Option<Blob>> {
        self.inner.get(key).await
    }

    async fn get_range(
        &self,
        key: &str,
        range: std::ops::Range<u64>,
    ) -> StorageResult<Option<Vec<u8>>> {
        self.inner.get_range(key, range).await
    }

    async fn head(&self, key: &str) -> StorageResult<Option<BlobMeta>> {
        self.inner.head(key).await
    }

    async fn delete(&self, key: &str) -> StorageResult<bool> {
        self.inner.delete(key).await
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<BlobMeta>> {
        self.inner.list(prefix).await
    }

    async fn list_page(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> StorageResult<BlobListPage> {
        let page = self.inner.list_page(prefix, after, limit).await?;
        if prefix == format!("{CCR_PREFIX}/") {
            let _ = tokio::time::timeout(Duration::from_millis(500), self.prune_reads.wait()).await;
        }
        Ok(page)
    }
}

#[test]
fn collection_reports_bytes_removed_by_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let store = BlobCcrStore::new(blobs, None).unwrap();
    assert!(store.put("first").retained());
    assert!(store.put("second").retained());

    assert_eq!(collect(dir.path(), None, Some(0)).unwrap(), (2, 11, 0, 0));
}

#[test]
fn collection_removes_and_reports_corrupt_legacy_entries() {
    let dir = tempfile::tempdir().unwrap();
    let token = "0123456789abcdef0123456789abcdef";
    let path = dir.path().join(token);
    let corrupt_content = "content does not match its token";
    std::fs::write(&path, corrupt_content).unwrap();

    let result = collect(dir.path(), None, Some(0)).unwrap();

    assert_eq!(result, (1, corrupt_content.len() as u64, 0, 0));
    assert!(
        !path.exists(),
        "garbage collection removes the corrupt file"
    );
}

#[test]
fn collection_reports_expired_legacy_entries() {
    let dir = tempfile::tempdir().unwrap();
    let content = "valid but expired legacy entry";
    let token = tinyjuice::cache::short_hash(content);
    let path = dir.path().join(token);
    std::fs::write(&path, content).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
        .unwrap();

    let result = collect(dir.path(), Some(Duration::from_secs(1)), None).unwrap();

    assert_eq!(result, (1, content.len() as u64, 0, 0));
    assert!(
        !path.exists(),
        "garbage collection removes expired legacy files"
    );
}

#[test]
fn collection_reports_expired_backend_blobs_pruned_during_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let content = "expired backend content swept during configuration";
    let store = BlobCcrStore::new(Arc::clone(&blobs), None).unwrap();
    let put = store.put(content);
    assert!(put.retained());
    write_semantic_time(
        Arc::clone(&blobs),
        put.token(),
        unix_epoch_ms(SystemTime::now() - Duration::from_secs(60)),
        None,
    )
    .unwrap();

    assert_eq!(
        collect(dir.path(), Some(Duration::from_secs(1)), None).unwrap(),
        (1, content.len() as u64, 0, 0)
    );
}

#[test]
fn collection_removes_and_reports_non_utf8_legacy_entries() {
    let dir = tempfile::tempdir().unwrap();
    let token = "fedcba9876543210fedcba9876543210";
    let path = dir.path().join(token);
    let corrupt_content = [0xff, 0xfe];
    std::fs::write(&path, corrupt_content).unwrap();

    let result = collect(dir.path(), None, Some(0)).unwrap();

    assert_eq!(result, (1, corrupt_content.len() as u64, 0, 0));
    assert!(
        !path.exists(),
        "garbage collection removes non-UTF-8 legacy files"
    );
}

#[test]
fn startup_prunes_expired_blobs_with_ttl_without_a_byte_cap() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let content = "expired before a TTL-only restart";
    let store = BlobCcrStore::new(Arc::clone(&blobs), None).unwrap();
    let put = store.put(content);
    assert!(put.retained());
    write_semantic_time(
        Arc::clone(&blobs),
        put.token(),
        unix_epoch_ms(SystemTime::now() - Duration::from_secs(60)),
        None,
    )
    .unwrap();

    configure_with_report(dir.path(), Some(Duration::from_secs(1)), None).unwrap();

    assert!(
        BlobCcrStore::new(open_file_blobs(dir.path()).unwrap(), None)
            .unwrap()
            .get(put.token())
            .is_none(),
        "a TTL-only startup sweep removes previously stored expired content"
    );
}

#[test]
fn configuration_reapplies_changed_ttl_to_existing_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let blobs = open_file_blobs(root).unwrap();
    let content = "an existing cache entry whose configured TTL changes";
    let token = tinyjuice::cache::short_hash(content);
    assert!(
        BlobCcrStore::new(Arc::clone(&blobs), Some(Duration::from_secs(3_600)))
            .unwrap()
            .put(content)
            .retained()
    );

    let body_sidecar = || {
        std::fs::read_dir(root.join("scopes/tinyjuice-cli/blobs"))
            .unwrap()
            .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
            .filter_map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .find(|sidecar| sidecar["key"] == format!("{CCR_PREFIX}/{token}"))
            .expect("CCR body metadata sidecar exists")
    };
    let original_expiry = body_sidecar()["expires_at_ms"]
        .as_u64()
        .expect("the initial TTL is recorded in the backend sidecar");

    configure(root, Some(Duration::from_secs(7_200)), None).unwrap();

    let extended_expiry = body_sidecar()["expires_at_ms"]
        .as_u64()
        .expect("the updated TTL is recorded in the backend sidecar");
    assert!(
        extended_expiry > original_expiry,
        "a longer configured TTL must replace the old physical expiry"
    );

    configure(root, None, None).unwrap();

    assert!(body_sidecar()["expires_at_ms"].is_null());
    assert_eq!(
        BlobCcrStore::new(open_file_blobs(root).unwrap(), None)
            .unwrap()
            .get(&token)
            .as_deref(),
        Some(content)
    );
}

#[test]
fn configuration_preserves_backend_write_time_when_semantic_time_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let blobs = open_file_blobs(root).unwrap();
    let content = "existing body without a semantic timestamp";
    let token = tinyjuice::cache::short_hash(content);
    assert!(
        BlobCcrStore::new(Arc::clone(&blobs), Some(Duration::from_secs(3_600)))
            .unwrap()
            .put(content)
            .retained()
    );
    let written_at_ms = list_all(Arc::clone(&blobs))
        .unwrap()
        .into_iter()
        .find(|entry| entry.meta.key == format!("{CCR_PREFIX}/{token}"))
        .and_then(|entry| entry.written_at_ms)
        .expect("file backend records the original body write time");

    configure(root, Some(Duration::from_secs(7_200)), None).unwrap();

    assert_eq!(
        read_semantic_time(open_file_blobs(root).unwrap(), &token).unwrap(),
        Some(written_at_ms),
        "reapplying expiry must preserve the original age for later pruning"
    );
}

#[test]
fn configuration_does_not_rewrite_cache_when_ttl_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let blobs = open_file_blobs(root).unwrap();
    let content = "cache body used to check unchanged TTL configuration";
    let token = tinyjuice::cache::short_hash(content);
    let ttl = Some(Duration::from_secs(3_600));
    assert!(
        BlobCcrStore::new(Arc::clone(&blobs), ttl)
            .unwrap()
            .put(content)
            .retained()
    );
    configure(root, ttl, None).unwrap();

    let body_sidecar = std::fs::read_dir(root.join("scopes/tinyjuice-cli/blobs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
                && std::fs::read(path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                    .is_some_and(|sidecar| sidecar["key"] == format!("{CCR_PREFIX}/{token}"))
        })
        .expect("CCR body metadata sidecar exists");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&body_sidecar)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
        .unwrap();

    configure(root, ttl, None).unwrap();

    assert_eq!(
        std::fs::metadata(body_sidecar).unwrap().modified().unwrap(),
        SystemTime::UNIX_EPOCH,
        "an unchanged TTL must not rewrite each cached body's metadata"
    );
}

#[test]
fn stale_capped_store_does_not_write_after_configuration_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let blobs = open_file_blobs(root).unwrap();
    let old_ttl = Some(Duration::from_secs(3_600));
    let new_ttl = Some(Duration::from_secs(7_200));
    configure(root, old_ttl, None).unwrap();
    let stale = CappedCcrStore {
        store: BlobCcrStore::new(Arc::clone(&blobs), old_ttl).unwrap(),
        blobs: Arc::clone(&blobs),
        lock_path: process_lock_path(root),
        ttl: old_ttl,
        max_bytes: None,
    };
    configure(root, new_ttl, None).unwrap();

    let result = stale.put("stale process must not write with an old TTL");

    assert!(!result.retained());
    assert!(
        run_blob(async move { blobs.list(&format!("{CCR_PREFIX}/")).await })
            .unwrap()
            .is_empty(),
        "a store configured with a superseded TTL must not publish a body"
    );
}

#[test]
fn removing_legacy_entries_reports_inspection_and_removal_errors() {
    let dir = tempfile::tempdir().unwrap();
    let token = "0123456789abcdef0123456789abcdef";
    let missing_path = dir.path().join(token);
    assert_eq!(
        remove_legacy_entry(&missing_path, token, "corrupt").unwrap(),
        None
    );

    let raced_path = dir.path().join("raced");
    std::fs::write(&raced_path, "legacy").unwrap();
    let removed = remove_legacy_entry_with(&raced_path, token, "corrupt", |path| {
        std::fs::remove_file(path)?;
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    })
    .unwrap();
    assert_eq!(removed, None);
    assert!(!raced_path.exists());

    let directory_path = dir.path().join(token);
    std::fs::create_dir(&directory_path).unwrap();
    let remove_error = remove_legacy_entry(&directory_path, token, "expired").unwrap_err();
    assert!(remove_error.contains("cannot remove expired legacy CCR entry"));
    assert!(directory_path.is_dir());
}

#[test]
fn imported_legacy_removal_tolerates_a_concurrent_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("imported");
    std::fs::write(&path, "legacy").unwrap();

    remove_imported_legacy_entry_with(&path, "0123456789abcdef0123456789abcdef", |path| {
        std::fs::remove_file(path)?;
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    })
    .unwrap();

    assert!(!path.exists());
}

#[test]
fn configure_and_inventory_report_invalid_storage_paths() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    let inventory_error = match inventory(&missing, None) {
        Err(error) => error,
        Ok(_) => panic!("missing storage directory unexpectedly inventoried"),
    };
    assert!(inventory_error.contains("does not exist"));

    let file_path = dir.path().join("not-a-directory");
    std::fs::write(&file_path, "file").unwrap();
    let configure_error = configure(&file_path, None, None).unwrap_err();
    assert!(configure_error.contains("cannot create CCR storage directory"));
    let import_error =
        import_legacy_files(&file_path, &open_file_blobs(dir.path()).unwrap(), None).unwrap_err();
    assert!(import_error.contains("failed to inspect legacy CCR directory"));
}

#[test]
fn legacy_import_skips_corrupt_entries_and_continues_importing_valid_files() {
    let dir = tempfile::tempdir().unwrap();
    let valid_content = "valid legacy entry still imports";
    let valid_token = tinyjuice::cache::short_hash(valid_content);
    let mismatch_token = "0123456789abcdef0123456789abcdef";
    let unreadable_token = "fedcba9876543210fedcba9876543210";
    let valid_path = dir.path().join(&valid_token);
    let mismatch_path = dir.path().join(mismatch_token);
    let unreadable_path = dir.path().join(unreadable_token);
    std::fs::write(&valid_path, valid_content).unwrap();
    std::fs::write(&mismatch_path, "content does not match its token").unwrap();
    std::fs::write(&unreadable_path, [0xff, 0xfe]).unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();

    import_legacy_files(dir.path(), &blobs, None).unwrap();

    assert!(!valid_path.exists(), "the valid legacy file is imported");
    assert!(
        !mismatch_path.exists(),
        "the corrupt source is discarded rather than retried forever"
    );
    assert!(
        !unreadable_path.exists(),
        "the non-UTF-8 source is discarded"
    );
    assert_eq!(
        BlobCcrStore::new(blobs, None)
            .unwrap()
            .get(&valid_token)
            .as_deref(),
        Some(valid_content)
    );
}

#[test]
fn inventory_counts_ignored_entries_and_returns_imported_blobs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("not-a-token"), "ignored").unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    let content = "inventory imports the valid legacy entry";
    let token = tinyjuice::cache::short_hash(content);
    std::fs::write(dir.path().join(&token), content).unwrap();

    let result = inventory(dir.path(), None).unwrap();

    assert_eq!(result.ignored_entries, 2);
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].key, format!("{CCR_PREFIX}/{token}"));
    assert_eq!(
        BlobCcrStore::new(open_file_blobs(dir.path()).unwrap(), None)
            .unwrap()
            .get(&token)
            .as_deref(),
        Some(content)
    );
}

#[test]
fn inventory_does_not_count_backend_storage_after_a_blob_write() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let store = BlobCcrStore::new(blobs, None).unwrap();
    assert!(store.put("inventory backend entry").retained());

    let first = inventory(dir.path(), None).unwrap();
    let second = inventory(dir.path(), None).unwrap();

    assert_eq!(first.ignored_entries, second.ignored_entries);
}

#[test]
fn concurrent_capped_writes_do_not_prune_each_other_into_an_empty_cache() {
    let dir = tempfile::tempdir().unwrap();
    let inner = open_file_blobs(dir.path()).unwrap();
    let body_writes = Arc::new(tokio::sync::Barrier::new(2));
    let prune_reads = Arc::new(tokio::sync::Barrier::new(2));
    let blobs: Arc<dyn BlobStore> = Arc::new(SynchronizeConcurrentPuts {
        inner: Arc::clone(&inner),
        body_writes,
        prune_reads,
    });
    let lock_path = dir.path().join("scopes/tinyjuice-cli/.tinyjuice-ccr.lock");
    let make_store = || CappedCcrStore {
        store: BlobCcrStore::new(Arc::clone(&blobs), None).unwrap(),
        blobs: Arc::clone(&blobs),
        lock_path: lock_path.clone(),
        ttl: None,
        max_bytes: Some(5),
    };
    let first = make_store();
    let second = make_store();
    let first_put = std::thread::spawn(move || first.put("first"));
    let second_put = std::thread::spawn(move || second.put("other"));

    let first_put = first_put.join().unwrap();
    let second_put = second_put.join().unwrap();
    assert!(first_put.retained());
    assert!(second_put.retained());
    let entries = run_blob(async move { inner.list(&format!("{CCR_PREFIX}/")).await }).unwrap();

    assert_eq!(
        entries.len(),
        1,
        "a capped concurrent write must retain one blob"
    );
}

#[test]
fn capped_write_fails_closed_when_process_lock_directory_cannot_be_created() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let blocker = dir.path().join("not-a-directory");
    std::fs::write(&blocker, b"file blocks lock directory creation").unwrap();
    let store = CappedCcrStore {
        store: BlobCcrStore::new(Arc::clone(&blobs), None).unwrap(),
        blobs: Arc::clone(&blobs),
        lock_path: blocker.join(".tinyjuice-ccr.lock"),
        ttl: None,
        max_bytes: Some(1024),
    };

    let result = store.put("must not be written without the process lock");

    assert!(!result.retained());
    assert!(
        run_blob(async move { blobs.list(&format!("{CCR_PREFIX}/")).await })
            .unwrap()
            .is_empty(),
        "the write must not proceed after process-lock acquisition fails"
    );
}

#[test]
fn import_skips_directories_and_unrecognized_names_and_removes_expired_files() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("entry"), "not a top-level CCR").unwrap();
    let unrecognized = dir.path().join("not-a-token");
    std::fs::write(&unrecognized, "unrecognized").unwrap();

    let content = "already expired legacy content";
    let token = tinyjuice::cache::short_hash(content);
    let expired = dir.path().join(&token);
    std::fs::write(&expired, content).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&expired)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
        .unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();

    import_legacy_files(dir.path(), &blobs, Some(Duration::from_secs(1))).unwrap();

    assert!(!expired.exists());
    assert!(unrecognized.exists());
    assert!(nested.join("entry").exists());
    assert!(
        BlobCcrStore::new(blobs, None)
            .unwrap()
            .get(&token)
            .is_none()
    );
}

#[test]
fn import_uses_an_existing_backend_write_time_without_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let content = "the already published backend body is newer";
    let token = tinyjuice::cache::short_hash(content);
    let source = dir.path().join(&token);
    std::fs::write(&source, content).unwrap();
    let source_time = SystemTime::now() - Duration::from_secs(60);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(source_time))
        .unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let existing = BlobCcrStore::new(Arc::clone(&blobs), None).unwrap();
    assert!(existing.put(content).retained());
    let backend_time = backend_written_at(Arc::clone(&blobs), &format!("{CCR_PREFIX}/{token}"))
        .unwrap()
        .unwrap();

    import_legacy_files(dir.path(), &blobs, None).unwrap();

    assert!(!source.exists());
    assert_eq!(
        read_semantic_time(blobs, &token).unwrap(),
        Some(backend_time.max(unix_epoch_ms(source_time)))
    );
}

#[test]
fn capped_store_delegates_byte_ranges_to_the_durable_store() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let store = CappedCcrStore {
        store: BlobCcrStore::new(Arc::clone(&blobs), None).unwrap(),
        blobs,
        lock_path: process_lock_path(dir.path()),
        ttl: None,
        max_bytes: None,
    };
    let put = store.put("abcdefgh");

    assert_eq!(
        store
            .get_range(put.token(), 2, 5, RangeUnit::Bytes)
            .as_deref(),
        Some("cde")
    );
}

#[test]
fn timestamp_listing_ignores_non_tokens_and_discards_malformed_values() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    run_blob({
        let blobs = Arc::clone(&blobs);
        async move {
            blobs
                .put(
                    &format!("{CCR_METADATA_PREFIX}not-a-token"),
                    b"not a timestamp".to_vec(),
                    None,
                )
                .await
        }
    })
    .unwrap();
    assert!(list_semantic_times(Arc::clone(&blobs)).unwrap().is_empty());

    let token = "0123456789abcdef0123456789abcdef";
    run_blob({
        let blobs = Arc::clone(&blobs);
        async move {
            blobs
                .put(&format!("{CCR_METADATA_PREFIX}{token}"), vec![0xff], None)
                .await
        }
    })
    .unwrap();
    assert_eq!(read_semantic_time(blobs, token).unwrap(), None);
}

#[test]
fn malformed_timestamp_sidecars_do_not_abort_pruning_or_hide_backend_write_time() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let store = BlobCcrStore::new(Arc::clone(&blobs), None).unwrap();
    let content = "recent content with malformed timestamp sidecars";
    let body_token = tinyjuice::cache::short_hash(content);
    assert!(store.put(content).retained());
    let body_token_for_sidecar = body_token.clone();
    let intent_token = "0123456789abcdef0123456789abcdef";
    run_blob({
        let blobs = Arc::clone(&blobs);
        async move {
            blobs
                .put(
                    &format!("{CCR_METADATA_PREFIX}{body_token_for_sidecar}"),
                    b"not-a-number".to_vec(),
                    None,
                )
                .await?;
            blobs
                .put(
                    &format!("{CCR_IMPORT_PREFIX}{intent_token}"),
                    vec![0xff],
                    None,
                )
                .await
        }
    })
    .unwrap();

    assert_eq!(
        prune(&blobs, Some(Duration::from_secs(3600)), None, None).unwrap(),
        (0, 0, 1, content.len() as u64)
    );
    assert_eq!(store.get(&body_token).as_deref(), Some(content));
    assert_eq!(
        read_semantic_time(Arc::clone(&blobs), &body_token).unwrap(),
        None
    );
    assert_eq!(read_import_intent(blobs, intent_token).unwrap(), None);
}

#[test]
fn malformed_timestamp_cleanup_failure_aborts_pruning() {
    let dir = tempfile::tempdir().unwrap();
    let inner = open_file_blobs(dir.path()).unwrap();
    let content = "body with an unreadable timestamp sidecar";
    let token = tinyjuice::cache::short_hash(content);
    assert!(
        BlobCcrStore::new(Arc::clone(&inner), None)
            .unwrap()
            .put(content)
            .retained()
    );
    let token_for_sidecar = token.clone();
    run_blob({
        let inner = Arc::clone(&inner);
        async move {
            inner
                .put(
                    &format!("{CCR_METADATA_PREFIX}{token_for_sidecar}"),
                    b"not-a-number".to_vec(),
                    None,
                )
                .await
        }
    })
    .unwrap();
    let faults = FaultPlan::new();
    faults.fail_operation_nth("semantic_metadata_delete", 1, ErrorKind::Backend);
    let failing: Arc<dyn BlobStore> = Arc::new(FailMetadataOperations {
        inner: Arc::clone(&inner),
        faults,
    });

    let error = prune(&failing, None, None, None).unwrap_err();

    assert!(error.contains("cannot remove malformed CCR timestamp sidecar"));
    assert!(
        run_blob(async move { inner.get(&format!("{CCR_METADATA_PREFIX}{token}")).await })
            .unwrap()
            .is_some()
    );
}

#[test]
fn timestamp_backend_read_errors_still_abort_pruning() {
    let dir = tempfile::tempdir().unwrap();
    let inner = open_file_blobs(dir.path()).unwrap();
    let token = "0123456789abcdef0123456789abcdef";
    write_semantic_time(
        Arc::clone(&inner),
        token,
        unix_epoch_ms(SystemTime::now()),
        None,
    )
    .unwrap();
    let faults = FaultPlan::new();
    faults.fail_operation_nth("metadata_get", 1, ErrorKind::Backend);
    let failing: Arc<dyn BlobStore> = Arc::new(FailMetadataOperations { inner, faults });

    let error = prune(&failing, None, None, None).unwrap_err();

    assert!(error.contains("injected failure for metadata_get"));
}

#[test]
fn deleting_a_non_ccr_blob_does_not_touch_ccr_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let key = "unrelated/object";
    run_blob({
        let blobs = Arc::clone(&blobs);
        async move { blobs.put(key, b"data".to_vec(), None).await }
    })
    .unwrap();

    delete_blob_and_metadata(Arc::clone(&blobs), key).unwrap();

    assert!(
        run_blob(async move { blobs.get(key).await })
            .unwrap()
            .is_none()
    );
}

#[test]
fn durable_cli_writes_enforce_the_configured_byte_budget() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let store = CappedCcrStore {
        store: BlobCcrStore::new(Arc::clone(&blobs), None).unwrap(),
        blobs: Arc::clone(&blobs),
        lock_path: process_lock_path(dir.path()),
        ttl: None,
        max_bytes: Some(9),
    };

    assert!(store.put("first").retained());
    assert!(store.put("second").retained());
    let current = store.put("tri");
    assert!(current.retained());

    let entries = run_blob({
        let blobs = Arc::clone(&blobs);
        async move { blobs.list(&format!("{CCR_PREFIX}/")).await }
    })
    .unwrap();
    assert_eq!(entries.iter().map(|entry| entry.len).sum::<u64>(), 9);
    assert!(store.get(current.token()).is_some());
}

#[test]
fn migration_ttl_uses_the_source_file_age() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(95);
    assert_eq!(
        remaining_ttl(modified, now, Duration::from_secs(10)),
        Some(Duration::from_secs(5))
    );
    assert_eq!(remaining_ttl(modified, now, Duration::from_secs(5)), None);
    assert_eq!(
        remaining_ttl(now + Duration::from_secs(1), now, Duration::from_secs(5)),
        Some(Duration::from_secs(5))
    );
}

#[test]
fn collection_uses_imported_write_time_for_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let content = "legacy ccr with an old semantic timestamp";
    let store = BlobCcrStore::new(Arc::clone(&blobs), None).unwrap();
    let put = store.put(content);
    assert!(put.retained());
    let old_time = unix_epoch_ms(SystemTime::UNIX_EPOCH + Duration::from_secs(1));
    write_semantic_time(Arc::clone(&blobs), put.token(), old_time, None).unwrap();

    assert_eq!(
        prune(&blobs, Some(Duration::from_secs(1)), None, None).unwrap(),
        (1, content.len() as u64, 0, 0)
    );
    assert!(store.get(put.token()).is_none());
    assert_eq!(
        read_semantic_time(Arc::clone(&blobs), put.token()).unwrap(),
        None
    );
}

#[test]
fn collection_orders_by_imported_write_time_under_the_byte_budget() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let store = BlobCcrStore::new(Arc::clone(&blobs), None).unwrap();
    let older = "older legacy ccr";
    let newer = "newer legacy ccr body";
    let older_put = store.put(older);
    let newer_put = store.put(newer);
    assert!(older_put.retained() && newer_put.retained());
    write_semantic_time(
        Arc::clone(&blobs),
        older_put.token(),
        unix_epoch_ms(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        None,
    )
    .unwrap();
    write_semantic_time(
        Arc::clone(&blobs),
        newer_put.token(),
        unix_epoch_ms(SystemTime::UNIX_EPOCH + Duration::from_secs(2)),
        None,
    )
    .unwrap();

    assert_eq!(
        prune(&blobs, None, Some(newer.len() as u64), None).unwrap(),
        (1, older.len() as u64, 1, newer.len() as u64)
    );
    assert!(store.get(older_put.token()).is_none());
    assert_eq!(store.get(newer_put.token()).as_deref(), Some(newer));
}

#[test]
fn gc_removes_import_intents_that_have_no_ccr_body() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let token = "0123456789abcdef0123456789abcdef";
    write_import_intent(
        Arc::clone(&blobs),
        token,
        unix_epoch_ms(SystemTime::now()),
        None,
    )
    .unwrap();

    assert_eq!(prune(&blobs, None, None, None).unwrap(), (0, 0, 0, 0));
    assert_eq!(read_import_intent(blobs, token).unwrap(), None);
}

#[test]
fn failed_timestamp_refresh_does_not_leave_an_old_age_for_gc() {
    let dir = tempfile::tempdir().unwrap();
    let content = "a normal re-offload of imported content";
    let token = tinyjuice::cache::short_hash(content);
    let inner = open_file_blobs(dir.path()).unwrap();
    let original = BlobCcrStore::new(Arc::clone(&inner), None).unwrap();
    assert!(original.put(content).retained());
    let old_time = unix_epoch_ms(SystemTime::now() - Duration::from_secs(86_400));
    write_semantic_time(Arc::clone(&inner), &token, old_time, None).unwrap();

    let faults = FaultPlan::new();
    faults.fail_operation_nth("metadata_put", 1, ErrorKind::Backend);
    let failing: Arc<dyn BlobStore> = Arc::new(FailMetadataOperations {
        inner: Arc::clone(&inner),
        faults,
    });
    let refreshed = CappedCcrStore {
        store: BlobCcrStore::new(Arc::clone(&failing), None).unwrap(),
        blobs: failing,
        lock_path: process_lock_path(dir.path()),
        ttl: None,
        max_bytes: None,
    };

    assert!(refreshed.put(content).retained());
    assert_eq!(
        read_semantic_time(Arc::clone(&inner), &token).unwrap(),
        None
    );
    assert_eq!(
        read_import_intent(Arc::clone(&inner), &token).unwrap(),
        None
    );
    assert_eq!(
        prune(&inner, Some(Duration::from_secs(3_600)), None, None).unwrap(),
        (0, 0, 1, content.len() as u64),
        "the physical write time must replace stale imported age when timestamp refresh fails"
    );
    assert_eq!(original.get(&token).as_deref(), Some(content));
}

#[test]
fn failed_stale_timestamp_cleanup_does_not_advertise_or_prune_the_new_body() {
    let dir = tempfile::tempdir().unwrap();
    let content = "fresh body whose stale semantic age cannot be cleared";
    let token = tinyjuice::cache::short_hash(content);
    let inner = open_file_blobs(dir.path()).unwrap();
    let original = BlobCcrStore::new(Arc::clone(&inner), None).unwrap();
    assert!(original.put(content).retained());
    let old_time = unix_epoch_ms(SystemTime::now() - Duration::from_secs(86_400));
    write_semantic_time(Arc::clone(&inner), &token, old_time, None).unwrap();

    let faults = FaultPlan::new();
    faults.fail_operation_nth("metadata_put", 1, ErrorKind::Backend);
    faults.fail_operation_nth("semantic_metadata_delete", 1, ErrorKind::Backend);
    let failing: Arc<dyn BlobStore> = Arc::new(FailMetadataOperations {
        inner: Arc::clone(&inner),
        faults,
    });
    let capped = CappedCcrStore {
        store: BlobCcrStore::new(Arc::clone(&failing), None).unwrap(),
        blobs: failing,
        lock_path: process_lock_path(dir.path()),
        ttl: Some(Duration::from_secs(3_600)),
        max_bytes: None,
    };

    assert!(!capped.put(content).retained());
    assert_eq!(
        read_semantic_time(Arc::clone(&inner), &token).unwrap(),
        Some(old_time)
    );
    assert_eq!(original.get(&token).as_deref(), Some(content));
}

#[test]
fn stale_import_intent_does_not_hide_a_newer_body_write() {
    let dir = tempfile::tempdir().unwrap();
    let content = "a later normal re-offload after an interrupted import";
    let token = tinyjuice::cache::short_hash(content);
    let source_path = dir.path().join(&token);
    std::fs::write(&source_path, content).unwrap();
    let old_time = SystemTime::now() - Duration::from_secs(60);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source_path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old_time))
        .unwrap();

    let inner = open_file_blobs(dir.path()).unwrap();
    let initial = BlobCcrStore::new(Arc::clone(&inner), None).unwrap();
    assert!(initial.put(content).retained());
    write_import_intent(Arc::clone(&inner), &token, unix_epoch_ms(old_time), None).unwrap();

    let faults = FaultPlan::new();
    faults.fail_operation_nth("body_put_after_commit", 1, ErrorKind::Backend);
    let flaky: Arc<dyn BlobStore> = Arc::new(FailMetadataOperations {
        inner: Arc::clone(&inner),
        faults,
    });
    let reoffloaded = CappedCcrStore {
        store: BlobCcrStore::new(Arc::clone(&flaky), None).unwrap(),
        blobs: flaky.clone(),
        lock_path: process_lock_path(dir.path()),
        ttl: None,
        max_bytes: None,
    };
    assert!(!reoffloaded.put(content).retained());
    let backend_time = backend_written_at(Arc::clone(&inner), &format!("{CCR_PREFIX}/{token}"))
        .unwrap()
        .unwrap();
    assert!(backend_time > unix_epoch_ms(old_time));

    import_legacy_files(dir.path(), &flaky, None).unwrap();

    assert_eq!(
        read_semantic_time(inner, &token).unwrap(),
        Some(backend_time)
    );
}

#[test]
fn capped_store_put_works_inside_a_current_thread_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let capped = CappedCcrStore {
        store: BlobCcrStore::new(Arc::clone(&blobs), None).unwrap(),
        blobs,
        lock_path: process_lock_path(dir.path()),
        ttl: Some(Duration::from_secs(600)),
        max_bytes: Some(1024),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let result = runtime.block_on(async { capped.put("retained from an active runtime") });

    assert!(result.retained());
}

#[test]
fn import_refuses_to_replace_different_content_at_a_legacy_token() {
    let dir = tempfile::tempdir().unwrap();
    let content = "source legacy content";
    let token = tinyjuice::cache::short_hash(content);
    let source_path = dir.path().join(&token);
    std::fs::write(&source_path, content).unwrap();
    let blobs = open_file_blobs(dir.path()).unwrap();
    let key = format!("{CCR_PREFIX}/{token}");
    run_blob({
        let blobs = Arc::clone(&blobs);
        let key = key.clone();
        async move {
            blobs
                .put(&key, b"different target content".to_vec(), None)
                .await
        }
    })
    .unwrap();

    let error = import_legacy_files(dir.path(), &blobs, None).unwrap_err();
    assert!(error.contains("conflicts with different stored content"));
    assert!(source_path.exists(), "the unimported source is retained");
    let existing = run_blob({
        let blobs = Arc::clone(&blobs);
        async move { blobs.get(&key).await }
    })
    .unwrap()
    .unwrap();
    assert_eq!(existing.bytes, b"different target content");
}

#[test]
fn retry_after_timestamp_write_failure_preserves_the_source_file_age() {
    let dir = tempfile::tempdir().unwrap();
    let content = "legacy ccr whose age must survive an interrupted import";
    let token = tinyjuice::cache::short_hash(content);
    let source_path = dir.path().join(&token);
    std::fs::write(&source_path, content).unwrap();
    let source_time = SystemTime::now() - Duration::from_secs(20);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source_path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(source_time))
        .unwrap();

    let inner = open_file_blobs(dir.path()).unwrap();
    let faults = FaultPlan::new();
    faults.fail_operation_nth("metadata_put", 1, ErrorKind::Backend);
    let failing: Arc<dyn BlobStore> = Arc::new(FailMetadataOperations {
        inner: Arc::clone(&inner),
        faults: faults.clone(),
    });

    assert!(import_legacy_files(dir.path(), &failing, None).is_err());
    assert!(source_path.exists(), "failed import must retain its source");
    let body_key = format!("{CCR_PREFIX}/{token}");
    assert!(
        run_blob({
            let inner = Arc::clone(&inner);
            let body_key = body_key.clone();
            async move { inner.get(&body_key).await }
        })
        .unwrap()
        .is_some(),
        "the fault occurs after the body has been stored"
    );

    faults.clear();
    import_legacy_files(dir.path(), &failing, None).unwrap();
    assert!(
        !source_path.exists(),
        "retry completes and retires the source"
    );
    assert_eq!(
        read_semantic_time(Arc::clone(&inner), &token).unwrap(),
        Some(unix_epoch_ms(source_time))
    );
    assert_eq!(
        read_import_intent(Arc::clone(&inner), &token).unwrap(),
        None
    );
}

#[test]
fn imports_released_v01010_disk_tier_file_and_preserves_its_age() {
    const TOKEN: &str = "1e66b4cff1835002102d821ffee64f75";
    const CONTENT: &str =
        include_str!("../tests/fixtures/legacy-ccr/v0.10.10/1e66b4cff1835002102d821ffee64f75");

    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join(TOKEN);
    std::fs::write(&source_path, CONTENT).unwrap();
    let source_time = SystemTime::now() - Duration::from_secs(5);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source_path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(source_time))
        .unwrap();

    assert_eq!(tinyjuice::cache::short_hash(CONTENT), TOKEN);
    let blobs = open_file_blobs(dir.path()).unwrap();
    import_legacy_files(dir.path(), &blobs, Some(Duration::from_secs(3600))).unwrap();

    assert!(!source_path.exists(), "the verified legacy file is retired");
    let store = BlobCcrStore::new(Arc::clone(&blobs), None).unwrap();
    assert_eq!(store.get(TOKEN).as_deref(), Some(CONTENT));
    assert_eq!(
        read_semantic_time(Arc::clone(&blobs), TOKEN).unwrap(),
        Some(unix_epoch_ms(source_time))
    );
}
