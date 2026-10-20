use tinyjuice_bus::STORAGE_NEEDS;

#[test]
fn host_can_import_current_storage_needs_without_the_implementation_crate() {
    let declared: &tinystoragedrivers_bus::StorageNeeds<'static> = &STORAGE_NEEDS;
    assert_eq!(declared.collections.len(), 1);
    assert_eq!(declared.collections[0].name, "juice.savings");
    assert_eq!(declared.blob_prefixes, &["juice/ccr/"]);
    assert!(declared.stream_prefixes.is_empty());
    assert!(declared.secret_prefixes.is_empty());
    assert_eq!(
        tinyjuice_storage_needs_consumer::storage_needs().blob_prefixes,
        declared.blob_prefixes
    );
}
