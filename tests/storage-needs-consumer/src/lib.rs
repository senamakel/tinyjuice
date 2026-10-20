//! Compile-time surface used to verify a host's bus-only dependency graph.

/// Return TinyJuice's host-grant metadata through the shared storage-bus type.
pub fn storage_needs() -> &'static tinystoragedrivers_bus::StorageNeeds<'static> {
    &tinyjuice_bus::STORAGE_NEEDS
}
