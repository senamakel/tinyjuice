//! TinyJuice's storage vocabulary and pure migration from legacy CCR keys.

use tinystoragedrivers_types::CollectionSpec;

/// CCR originals use the owner-scoped `juice/ccr/<token>` blob keys.
pub const CCR_PREFIX: &str = "juice/ccr";

/// The complete blob-prefix declaration for CCR originals.
pub const CCR_BLOB_PREFIX: &str = "juice/ccr/";

/// Legacy key prefix used by releases before the owner-scoped key migration.
pub const LEGACY_CCR_PREFIX: &str = "tinyjuice_ccr/";

/// The owner-scoped savings totals document collection.
pub const SAVINGS_COLLECTION: &str = "juice.savings";

/// Collections a host should provision when it composes TinyJuice storage.
///
/// Blob prefixes are provisioned by the blob engine itself; only document
/// collections need a declaration in this vocabulary.
#[must_use]
pub fn collections() -> Vec<CollectionSpec> {
    vec![CollectionSpec::new(SAVINGS_COLLECTION)]
}

/// Move one legacy key below the owner's current prefix without changing its
/// body or content type. Returns `None` for keys outside the legacy prefix.
#[must_use]
pub fn migrate_ccr_blob(
    key: String,
    content_type: Option<String>,
    bytes: Vec<u8>,
) -> Option<(String, Option<String>, Vec<u8>)> {
    let rest = key.strip_prefix(LEGACY_CCR_PREFIX)?;
    Some((format!("{CCR_PREFIX}/{rest}"), content_type, bytes))
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
