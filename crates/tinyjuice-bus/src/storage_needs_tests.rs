use super::*;

#[test]
fn storage_needs_declares_current_savings_and_ccr_resources() {
    assert_eq!(STORAGE_NEEDS.blob_prefixes, &[CCR_BLOB_PREFIX]);
    assert_eq!(STORAGE_NEEDS.collections.len(), 1);
    assert_eq!(STORAGE_NEEDS.collections[0].name, SAVINGS_COLLECTION);
    assert!(STORAGE_NEEDS.stream_prefixes.is_empty());
    assert!(STORAGE_NEEDS.secret_prefixes.is_empty());
}
