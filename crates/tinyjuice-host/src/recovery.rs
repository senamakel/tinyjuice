//! Recovery-name recognition and compaction result helpers for hosts.

/// Every accepted recovery tool spelling, including names in older transcripts.
pub const RECOVERY_TOOL_NAMES: &[&str] = &[
    tinyjuice_bus::tools::RETRIEVE,
    tinyjuice_bus::tools::LEGACY_RETRIEVE,
    "tokenjuice_retrieve",
    "retrieve_tool_output",
];

/// The sole recovery tool that needs to appear on a fresh model-facing belt.
pub const RECOVERY_TOOL_VISIBLE: &[&str] = &[tinyjuice_bus::tools::RETRIEVE];

/// Whether `name` is a canonical or legacy recovery tool name.
pub fn is_recovery_tool(name: &str) -> bool {
    RECOVERY_TOOL_NAMES.contains(&name)
}

/// The host-facing result of compaction or pass-through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactedToolOutput {
    pub text: String,
    pub notice: Option<String>,
    pub summarized_from_bytes: Option<usize>,
}

impl CompactedToolOutput {
    /// An output whose text was unchanged by compaction.
    pub fn unchanged(text: String) -> Self {
        Self {
            text,
            notice: None,
            summarized_from_bytes: None,
        }
    }

    /// The original output, with a notice when a requested summary failed.
    pub fn passthrough(text: String, wants_summary: bool) -> Self {
        Self {
            notice: wants_summary.then(summary_failed_notice),
            ..Self::unchanged(text)
        }
    }
}

/// Notice shared with the module for an unavailable summary.
pub fn summary_failed_notice() -> String {
    tinyjuice_bus::summary::UnavailableReason::Failed
        .notice()
        .to_owned()
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
