use super::*;

#[test]
fn storage_needs_declares_the_ready_to_grant_ccr_blob_prefix() {
    assert_eq!(STORAGE_NEEDS.blob_prefixes, &["tinyjuice_ccr/"]);
    assert!(STORAGE_NEEDS.collections.is_empty());
    assert!(STORAGE_NEEDS.stream_prefixes.is_empty());
    assert!(STORAGE_NEEDS.secret_prefixes.is_empty());
}
