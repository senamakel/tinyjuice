//! Compile-time surface used to verify a host's bus-only dependency graph.

use tinyjuice_bus::StorageNeeds;

/// Return TinyJuice's public storage declaration through the published contract.
pub fn storage_needs() -> StorageNeeds<'static> {
    tinyjuice_bus::STORAGE_NEEDS
}
