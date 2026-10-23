use super::*;

#[test]
fn owner_collection_and_blob_key_contracts_are_stable() {
    assert_eq!(collections(), vec![CollectionSpec::new("juice.savings")]);
    assert_eq!(CCR_PREFIX, "juice/ccr");
    assert_eq!(CCR_BLOB_PREFIX, "juice/ccr/");
    assert_eq!(LEGACY_CCR_PREFIX, "tinyjuice_ccr/");
}

#[test]
fn ccr_key_migration_preserves_payload_and_metadata() {
    let migrated = migrate_ccr_blob(
        "tinyjuice_ccr/0123456789abcdef".into(),
        Some("text/plain; charset=utf-8".into()),
        b"original text".to_vec(),
    )
    .unwrap();
    assert_eq!(migrated.0, "juice/ccr/0123456789abcdef");
    assert_eq!(migrated.1.as_deref(), Some("text/plain; charset=utf-8"));
    assert_eq!(migrated.2, b"original text");
}

#[test]
fn unrelated_blob_keys_are_not_migrated() {
    let moved = migrate_ccr_blob("elsewhere/x".into(), None, b"x".to_vec());
    assert!(moved.is_none());
}

#[test]
fn the_savings_body_is_snake_case_with_ms_times_and_round_trips() {
    use crate::savings::{SavingsAggregate, SavingsBucket, StoredSavings};
    let mut aggregate = SavingsAggregate::default();
    let bucket = SavingsBucket {
        events: 1,
        original_tokens: 200,
        compacted_tokens: 100,
        tokens_saved: 100,
        cost_saved_usd: 0.5,
    };
    aggregate.total = bucket.clone();
    aggregate.by_model.insert("m".into(), bucket);
    let body = serde_json::to_value(StoredSavings::from_aggregate(&aggregate, 1234)).unwrap();
    assert_eq!(body["total"]["tokens_saved"], 100);
    assert_eq!(body["by_model"]["m"]["original_tokens"], 200);
    assert_eq!(body["updated_ms"], 1234);
    assert!(body["total"].get("tokensSaved").is_none());
    let back: StoredSavings = serde_json::from_value(body).unwrap();
    assert_eq!(back.into_aggregate().by_model["m"].compacted_tokens, 100);
}
