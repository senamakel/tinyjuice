use super::*;

#[test]
fn every_shipped_and_migration_name_is_a_recovery_tool() {
    assert_eq!(
        RECOVERY_TOOL_NAMES,
        [
            "juice_retrieve",
            "tinyjuice_retrieve",
            "tokenjuice_retrieve",
            "retrieve_tool_output"
        ]
    );
    for name in RECOVERY_TOOL_NAMES {
        assert!(is_recovery_tool(name));
    }
    assert_eq!(RECOVERY_TOOL_VISIBLE, ["juice_retrieve"]);
    assert!(!is_recovery_tool("shell"));
}

#[test]
fn pass_through_discloses_only_a_requested_summary_failure() {
    let unchanged = CompactedToolOutput::unchanged("raw".into());
    assert_eq!(unchanged.text, "raw");
    assert!(unchanged.notice.is_none());

    let failed = CompactedToolOutput::passthrough("raw".into(), true);
    assert_eq!(failed.text, "raw");
    assert_eq!(failed.notice, Some(summary_failed_notice()));
    assert!(failed.summarized_from_bytes.is_none());
}
