use super::*;

#[test]
fn install_request_names_host_durable_ccr_without_exposing_a_path() {
    let request = InstallRequest {
        options: CompressOptions::default(),
        max_cache_entries: 8,
        max_cache_bytes: 1024,
        ccr_ttl_secs: None,
        ccr_durable: true,
    };

    let value = serde_json::to_value(request).unwrap();
    assert_eq!(value["ccrDurable"], true);
    assert!(value.get("diskTierRoot").is_none());
}

#[test]
fn install_request_defaults_to_no_host_durable_ccr() {
    let request = serde_json::json!({
        "options": {},
        "maxCacheEntries": 8,
        "maxCacheBytes": 1024,
        "ccrTtlSecs": null
    });

    let parsed: InstallRequest = serde_json::from_value(request).unwrap();
    assert!(!parsed.ccr_durable);
}
