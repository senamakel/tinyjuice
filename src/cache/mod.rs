//! CCR (Compress-Cache-Retrieve) — original storage + retrieval markers.

#[cfg(all(test, feature = "cli-disk-tier"))]
pub(crate) static DISK_TIER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(feature = "cli-disk-tier")]
mod disk_tier;
pub mod marker;
pub mod store;

pub use marker::{
    LEGACY_RETRIEVE_TOOL_NAME, NEVER_COMPACT_TOOLS, RECOVERY_TOOL_NAMES, RETRIEVE_TOOL_NAME,
    format_marker, is_recovery_tool, parse_markers, recovery_footer, recovery_footer_with,
};
pub use store::{
    CcrPutResult, CcrStore, DEFAULT_MAX_BYTES, DEFAULT_MAX_ENTRIES, GlobalCcrStore, MemoryCcrStore,
    RangeUnit, configure, offload, offload_checked, offload_checked_with_hash, retrieve,
    retrieve_range, set_durable_tier, short_hash, stats,
};

#[cfg(feature = "cli-disk-tier")]
pub use disk_tier::{
    DEFAULT_DISK_MAX_BYTES, GcStats, configure_disk_cap, disable_disk_tier, enable_disk_tier,
    gc_disk_dir, gc_disk_tier,
};
