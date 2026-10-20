//! Canonical names for TinyJuice's persisted resources.

/// Base path of CCR originals, without the token or trailing slash.
pub const CCR_PREFIX: &str = "juice/ccr";

/// Blob namespace declared for CCR originals, including its grant boundary.
pub const CCR_BLOB_PREFIX: &str = "juice/ccr/";

/// Collection containing the module's savings totals.
pub const SAVINGS_COLLECTION: &str = "juice.savings";

/// v1 blob namespace retained as a source for the schema migration.
pub const LEGACY_CCR_BLOB_PREFIX: &str = "tinyjuice_ccr/";
