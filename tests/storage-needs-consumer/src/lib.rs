//! Compile-time surface used to verify a host's bus-only dependency graph.

/// Exact canonical storage schema used by TinyJuice's bus re-export.
pub const STORAGE_NEEDS: tinystoragedrivers_bus::StorageNeeds<'static> =
    tinyjuice_bus::STORAGE_NEEDS;

/// Return TinyJuice's public storage declaration through the published contract.
pub fn storage_needs() -> tinystoragedrivers_bus::StorageNeeds<'static> {
    STORAGE_NEEDS
}
