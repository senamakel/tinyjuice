use super::*;
use crate::blob_store::CCR_PREFIX;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tinystoragedrivers_core::schema::{Item, MigrateOptions, Registry, ScopeClass};
use tinystoragedrivers_core::{MemoryStorage, Scope, StorageBackend};

const LEGACY: &str = "tinyjuice_ccr/";

#[test]
fn the_schema_declares_the_ccr_blobs_and_the_savings_collection_at_module_scope() {
    let schema = storage_schema();
    let needs = &*tinyjuice_bus::STORAGE_NEEDS;
    assert_eq!(needs.blob_prefixes, &[CCR_BLOB_PREFIX]);
    assert_eq!(needs.collections.len(), 1);
    assert_eq!(needs.collections[0].name, SAVINGS_COLLECTION);
    let items: Vec<_> = schema.items.iter().collect();
    assert_eq!(items.len(), 2, "CCR blobs and savings are all juice stores");
    match items[0] {
        Item::Blobs { prefix, scope } => {
            assert_eq!(prefix, CCR_BLOB_PREFIX);
            assert_eq!(*scope, ScopeClass::Module);
        }
        other => panic!("expected a blob prefix, got {other:?}"),
    }
    match items[1] {
        Item::Collection {
            name,
            version,
            scope,
            ..
        } => {
            assert_eq!(name, SAVINGS_COLLECTION);
            assert_eq!(*version, 1);
            assert_eq!(*scope, ScopeClass::Module);
        }
        other => panic!("expected the savings collection, got {other:?}"),
    }
    assert!(
        format!("{CCR_PREFIX}/token").starts_with(CCR_BLOB_PREFIX),
        "keys the store writes sit under the declared prefix"
    );
    Registry::compose([schema]).expect("the schema composes");
}

#[tokio::test]
async fn migration_moves_v1_ccr_blobs_under_the_v2_prefix() {
    let backend = MemoryStorage::new();
    let storage = backend.for_scope(&Scope::local()).unwrap();
    let blobs = storage.blobs();
    blobs
        .put(
            "tinyjuice_ccr/0123456789abcdef",
            b"original text".to_vec(),
            Some("text/plain; charset=utf-8"),
        )
        .await
        .unwrap();
    blobs
        .put("tinyjuice_ccr/fedcba9876543210", b"two".to_vec(), None)
        .await
        .unwrap();
    // Not ours: must be left alone.
    blobs
        .put("other/keep", b"keep".to_vec(), None)
        .await
        .unwrap();

    let registry = Registry::compose([storage_schema()]).unwrap();
    let report = registry
        .migrate(&storage, &MigrateOptions::new())
        .await
        .unwrap();
    assert_eq!(report.ran[0].written, 2);

    assert!(blobs.list(LEGACY).await.unwrap().is_empty());
    let moved = blobs
        .get("juice/ccr/0123456789abcdef")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.bytes, b"original text");
    assert_eq!(
        moved.meta.content_type.as_deref(),
        Some("text/plain; charset=utf-8")
    );
    let moved = blobs
        .get("juice/ccr/fedcba9876543210")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.meta.content_type, None);
    assert!(blobs.get("other/keep").await.unwrap().is_some());

    let again = registry
        .migrate(&storage, &MigrateOptions::new())
        .await
        .unwrap();
    assert!(again.ran.is_empty(), "a finished step does not run again");
}

#[tokio::test]
async fn migration_preserves_legacy_ccr_blob_expiry_and_does_not_resurrect_expired_blobs() {
    use std::time::Duration;

    let now = Arc::new(AtomicU64::new(1_000));
    let clock = Arc::clone(&now);
    let backend = MemoryStorage::with_clock(Arc::new(move || clock.load(Ordering::SeqCst)));
    let storage = backend.for_scope(&Scope::local()).unwrap();
    let blobs = storage.blobs();
    blobs
        .put_expiring(
            "tinyjuice_ccr/live",
            b"still live".to_vec(),
            Some("text/plain"),
            Duration::from_millis(5_000),
        )
        .await
        .unwrap();
    blobs
        .put_expiring(
            "tinyjuice_ccr/expired",
            b"already expired".to_vec(),
            Some("text/plain"),
            Duration::from_millis(500),
        )
        .await
        .unwrap();
    blobs
        .put(
            "tinyjuice_ccr/persistent",
            b"no expiry".to_vec(),
            Some("text/plain"),
        )
        .await
        .unwrap();

    now.store(4_000, Ordering::SeqCst);
    Registry::compose([storage_schema()])
        .unwrap()
        .migrate(&storage, &MigrateOptions::new())
        .await
        .unwrap();

    now.store(5_999, Ordering::SeqCst);
    let live = blobs.get("juice/ccr/live").await.unwrap().unwrap();
    assert_eq!(live.bytes, b"still live");
    assert_eq!(live.meta.content_type.as_deref(), Some("text/plain"));
    assert!(blobs.get("juice/ccr/expired").await.unwrap().is_none());
    assert!(blobs.get("juice/ccr/persistent").await.unwrap().is_some());

    now.store(6_000, Ordering::SeqCst);
    assert!(
        blobs.get("juice/ccr/live").await.unwrap().is_none(),
        "migration must preserve the original absolute expiry at 6000 ms"
    );
    assert!(blobs.get("juice/ccr/expired").await.unwrap().is_none());
    assert!(blobs.get("juice/ccr/persistent").await.unwrap().is_some());
}

#[test]
fn a_key_outside_the_legacy_prefix_has_nothing_to_move() {
    let moved = move_ccr_blob("elsewhere/x".into(), None, b"x".to_vec()).unwrap();
    assert!(moved.is_none());
    let (key, content_type, bytes) = move_ccr_blob(
        "tinyjuice_ccr/ab".into(),
        Some("text/plain".into()),
        b"x".to_vec(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(key, "juice/ccr/ab");
    assert_eq!(content_type.as_deref(), Some("text/plain"));
    assert_eq!(bytes, b"x");
}
