use tinyjuice_bus::STORAGE_NEEDS;

#[test]
fn host_can_import_the_declared_ccr_prefix_without_the_implementation_crate() {
    assert_eq!(STORAGE_NEEDS.blob_prefixes, &["tinyjuice_ccr/"]);
    assert_eq!(
        tinyjuice_storage_needs_consumer::storage_needs().blob_prefixes,
        STORAGE_NEEDS.blob_prefixes
    );
}
