use tinyjuice_bus::STORAGE_NEEDS;

#[test]
fn host_can_import_the_declared_ccr_prefix_without_the_implementation_crate() {
    let declared: tinystoragedrivers_bus::StorageNeeds<'static> = STORAGE_NEEDS;
    assert!(declared.collections.is_empty());
    assert_eq!(declared.blob_prefixes, &["tinyjuice_ccr/"]);
    assert!(declared.stream_prefixes.is_empty());
    assert!(declared.secret_prefixes.is_empty());
    assert_eq!(
        tinyjuice_storage_needs_consumer::storage_needs().blob_prefixes,
        declared.blob_prefixes
    );
}
