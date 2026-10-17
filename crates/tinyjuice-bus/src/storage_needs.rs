//! Host-grantable resources required by TinyJuice.

use crate::StorageNeeds;

/// Blob namespace for CCR originals. The trailing slash keeps grants scoped to
/// the existing `tinyjuice_ccr/<token>` keys without matching sibling names.
const CCR_BLOB_PREFIX: &str = "tinyjuice_ccr/";

/// Storage resources TinyJuice needs from a host.
///
/// This declaration is metadata for host policy; it does not grant access.
pub const STORAGE_NEEDS: StorageNeeds<'static> = StorageNeeds {
    collections: &[],
    blob_prefixes: &[CCR_BLOB_PREFIX],
    stream_prefixes: &[],
    secret_prefixes: &[],
};
