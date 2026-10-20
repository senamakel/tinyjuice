//! Host-grantable resources required by TinyJuice.

use std::sync::LazyLock;

use crate::StorageNeeds;
use crate::storage_names::{CCR_BLOB_PREFIX, SAVINGS_COLLECTION};
use tinystoragedrivers_bus::CollectionSpec;

static SAVINGS_COLLECTIONS: LazyLock<[CollectionSpec; 1]> = LazyLock::new(|| {
    [CollectionSpec {
        name: SAVINGS_COLLECTION.into(),
        indexes: Vec::new(),
        ttl_field: None,
        search: None,
    }]
});

/// Storage resources TinyJuice needs from a host.
///
/// This declaration is metadata for host policy; it does not grant access.
pub static STORAGE_NEEDS: LazyLock<StorageNeeds<'static>> = LazyLock::new(|| StorageNeeds {
    collections: &SAVINGS_COLLECTIONS[..],
    blob_prefixes: &[CCR_BLOB_PREFIX],
    stream_prefixes: &[],
    secret_prefixes: &[],
});

#[cfg(test)]
#[path = "storage_needs_tests.rs"]
mod tests;
