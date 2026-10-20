//! TinyJuice's executable storage schema and v1 migration.
//!
//! The savings totals are one document in `juice.savings`.
//!
//! The CCR store's originals are blobs under `juice/ccr/<token>`. The module
//! runs for one tenant at a time, so they live at [`ScopeClass::Module`]. The
//! bus crate separately declares the host-facing `STORAGE_NEEDS` data.
//!
//! Before schema v2 the same blobs lived under `tinyjuice_ccr/<token>`. One
//! blob step moves them; the bytes and content type are untouched. The steps
//! are pure: no I/O happens until the host's registry runs them.
//!
//! TSD v1.8.0's runner carries each source's absolute expiry around the
//! existing `(key, content type, bytes)` transform tuple and writes that same
//! deadline at the target key.

use tinyjuice_bus::storage::{CCR_BLOB_PREFIX, LEGACY_CCR_BLOB_PREFIX, SAVINGS_COLLECTION};
use tinystoragedrivers_core::Result;
use tinystoragedrivers_core::schema::{BlobOutput, Owner, Schema, ScopeClass, Step};

/// The schema owner, and the first path segment of every blob key.
const OWNER: &str = "juice";

/// Everything TinyJuice stores, for the host's `Registry::compose`.
///
/// # Panics
///
/// Never in practice: the owner name is a constant that satisfies the owner
/// grammar, which the tests pin.
#[must_use]
pub fn storage_schema() -> Schema {
    Schema::new(Owner::new(OWNER).expect("`juice` is a valid schema owner"))
        .blobs(CCR_BLOB_PREFIX, ScopeClass::Module)
        .collection(SAVINGS_COLLECTION, 1, ScopeClass::Module)
        .step(ccr_prefix_step())
}

/// v1 to v2: `tinyjuice_ccr/<token>` becomes `juice/ccr/<token>`.
fn ccr_prefix_step() -> Step {
    Step::blobs("ccr_prefix_v2", CCR_BLOB_PREFIX, 1, 2, move_ccr_blob)
        .source_prefix(LEGACY_CCR_BLOB_PREFIX)
}

/// One blob of the v1 prefix, re-keyed. Bytes and content type are untouched.
fn move_ccr_blob(
    key: String,
    content_type: Option<String>,
    bytes: Vec<u8>,
) -> Result<Option<BlobOutput>> {
    // The runner lists by the legacy prefix, so every key carries it; a key
    // that somehow does not has nothing to move.
    let Some(rest) = key.strip_prefix(LEGACY_CCR_BLOB_PREFIX) else {
        return Ok(None);
    };
    Ok(Some((
        format!("{CCR_BLOB_PREFIX}{rest}"),
        content_type,
        bytes,
    )))
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
