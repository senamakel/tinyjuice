use super::*;
use crate::ShellCompactionPolicy;
use crate::types::CompressorKind;

fn opts() -> CompressOptions {
    CompressOptions {
        min_bytes_to_compress: 64,
        ..Default::default()
    }
}

#[tokio::test]
async fn routes_json_and_offloads() {
    let mut rows = Vec::new();
    for i in 0..120 {
        rows.push(format!(
            r#"{{"id":{i},"name":"account_{i}","email":"a{i}@ex.com","tier":"gold"}}"#
        ));
    }
    let original = format!("[{}]", rows.join(","));
    let res = compress_content(&original, None, &opts()).await;
    assert!(res.applied);
    assert_eq!(res.content_kind, ContentKind::Json);
    assert_eq!(res.compressor, CompressorKind::SmartCrusher);
    assert!(res.text.len() < original.len());
    let token = res.ccr_token.expect("offloaded");
    assert_eq!(cache::retrieve(&token).as_deref(), Some(original.as_str()));
    assert!(
        res.text.contains("juice_retrieve"),
        "footer marker present: {}",
        res.text
    );
}

#[tokio::test]
async fn detected_kind_routes_to_its_compressor_and_lossy_output_is_recoverable() {
    use crate::cache::CcrStore;
    use crate::types::ContentKind;

    let mut options = opts();
    options.ccr_min_tokens = 1;
    let store = cache::MemoryCcrStore::new(16, 1_000_000);
    let cases = [
        (
            ContentKind::Json,
            format!(
                "[{}]",
                (0..160)
                    .map(|i| format!(r#"{{"id":{i},"name":"account_{i}","status":"active"}}"#))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
        (
            ContentKind::Code,
            format!(
                "pub fn visible() {{\n{}\n}}\n",
                (0..40)
                    .map(|i| format!("    println!(\"account {i}\");"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        ),
        (
            ContentKind::Log,
            (0..120)
                .map(|i| format!("ERROR worker-{i} failed request with retryable timeout after repeated attempts\n"))
                .collect::<String>(),
        ),
        (
            ContentKind::Search,
            (0..120)
                .map(|i| format!("src/module.rs:{}:fn matching_result_{i}() {{}}\n", i + 1))
                .collect::<String>(),
        ),
        (
            ContentKind::Diff,
            format!(
                "diff --git a/Cargo.lock b/Cargo.lock\n@@ -1,300 +1,300 @@\n{}",
                (0..300)
                    .map(|i| format!("+ dependency_{i} = \"{}.{}.{}\"\n", i % 4, i % 20, i))
                    .collect::<String>()
            ),
        ),
        (
            ContentKind::Html,
            format!(
                "<!doctype html><html><body>{}</body></html>",
                (0..120)
                    .map(|i| format!("<section><h2>Result {i}</h2><p>Repeated page content for item {i}</p></section>"))
                    .collect::<String>()
            ),
        ),
        (
            ContentKind::PlainText,
            format!(
                "{}ERROR sync.worker.v2 failed for REQUEST_ID 9F42 after retry 17.\n{}",
                (0..80)
                    .map(|i| format!("Routine update {i}: the worker completed another ordinary queue item successfully.\n"))
                    .collect::<String>(),
                (80..160)
                    .map(|i| format!("Routine update {i}: the worker completed another ordinary queue item successfully.\n"))
                    .collect::<String>()
            ),
        ),
    ];

    for (expected_kind, content) in cases {
        let hint = ContentHint {
            extension: (expected_kind == ContentKind::Code).then(|| "rs".to_string()),
            query: match expected_kind {
                ContentKind::Search => Some("matching_result".to_string()),
                ContentKind::PlainText => Some("sync.worker.v2 REQUEST_ID".to_string()),
                _ => None,
            },
            ..Default::default()
        };
        let detected = detect_content_kind(&content, &hint);
        assert_eq!(detected, expected_kind, "fixture for {expected_kind:?}");

        let output = compress_content_with_store(&content, Some(hint), &options, &store).await;
        assert_eq!(
            output.content_kind, detected,
            "compress route for {expected_kind:?}"
        );
        assert!(
            output.applied,
            "fixture should exercise {expected_kind:?} compression"
        );
        assert_eq!(
            output.compressor,
            compressor_for(detected).kind(),
            "compressor route for {expected_kind:?}"
        );
        if output.lossy {
            let token = output
                .ccr_token
                .as_deref()
                .expect("lossy output retains CCR token");
            assert_eq!(
                store.get(token).as_deref(),
                Some(content.as_str()),
                "lossy output for {expected_kind:?} must recover every original byte"
            );
        }
    }
}

#[tokio::test]
async fn diff_noise_output_offloads_original_for_recovery() {
    let mut original = String::from("diff --git a/Cargo.lock b/Cargo.lock\n");
    original.push_str("@@ -1,800 +1,900 @@\n");
    for i in 0..700 {
        original.push_str(&format!("+ new dependency entry {i} checksum {}\n", i * 17));
    }
    for i in 0..350 {
        original.push_str(&format!("- old dependency entry {i} checksum {}\n", i * 19));
    }
    let hint = ContentHint {
        explicit: Some(ContentKind::Diff),
        ..Default::default()
    };
    let store = cache::MemoryCcrStore::new(10, 1_000_000);

    let res = compress_content_with_store(&original, Some(hint), &opts(), &store).await;

    assert!(res.applied);
    assert!(res.lossy);
    assert_eq!(res.content_kind, ContentKind::Diff);
    assert!(res.text.contains("reason=lockfile"), "{}", res.text);
    let token = res.ccr_token.as_deref().expect("offloaded");
    assert_eq!(store.get(token).as_deref(), Some(original.as_str()));
}

#[tokio::test]
async fn textcrusher_output_offloads_original_for_recovery() {
    let mut original = String::new();
    for i in 0..80 {
        original.push_str(&format!(
            "routine service note {i} describes ordinary progress through the worker queue.\n\n"
        ));
    }
    original.push_str(
        "ERROR sync.worker.v2 failed for REQUEST_ID 9F42 after retry 17 in api.worker.example.\n\n",
    );
    for i in 80..160 {
        original.push_str(&format!(
            "routine service note {i} describes ordinary progress through the worker queue.\n\n"
        ));
    }
    let hint = ContentHint {
        explicit: Some(ContentKind::PlainText),
        query: Some("sync.worker.v2 REQUEST_ID".to_string()),
        ..Default::default()
    };
    let store = cache::MemoryCcrStore::new(10, 1_000_000);
    let mut opts = opts();
    opts.ccr_min_tokens = 1;

    let res = compress_content_with_store(&original, Some(hint), &opts, &store).await;

    assert!(res.applied);
    assert!(res.lossy);
    assert_eq!(res.content_kind, ContentKind::PlainText);
    assert_eq!(res.compressor, CompressorKind::TextCrusher);
    assert!(
        res.body.contains("ERROR sync.worker.v2 failed"),
        "{}",
        res.body
    );
    let token = res.ccr_token.as_deref().expect("offloaded");
    assert_eq!(store.get(token).as_deref(), Some(original.as_str()));
}

#[tokio::test]
async fn exposes_body_and_recovery_footer_separately() {
    let mut rows = Vec::new();
    for i in 0..120 {
        rows.push(format!(
            r#"{{"id":{i},"name":"isolated_account_{i}","email":"iso{i}@ex.com","tier":"platinum"}}"#
        ));
    }
    let original = format!("[{}]", rows.join(","));
    let res = compress_content(&original, None, &opts()).await;

    assert!(res.applied);
    let footer = res
        .recovery_footer
        .as_deref()
        .expect("offloaded output exposes footer");
    assert!(
        !res.body.contains("juice_retrieve"),
        "body must not contain footer"
    );
    assert!(
        footer.contains("juice_retrieve"),
        "footer carries marker: {footer}"
    );
    assert_eq!(res.text, format!("{}{}", res.body, footer));
    assert_eq!(res.compacted_bytes, res.text.len());
}

#[tokio::test]
async fn store_injected_route_uses_isolated_ccr_store() {
    let store = cache::MemoryCcrStore::new(10, 1_000_000);
    let mut rows = Vec::new();
    for i in 0..120 {
        rows.push(format!(
            r#"{{"id":{i},"name":"store_only_account_{i}","email":"store{i}@ex.com","tier":"silver"}}"#
        ));
    }
    let original = format!("[{}]", rows.join(","));
    let res = compress_content_with_store(&original, None, &opts(), &store).await;

    assert!(res.applied);
    let token = res.ccr_token.as_deref().expect("offloaded");
    assert_eq!(store.get(token).as_deref(), Some(original.as_str()));
    assert_eq!(cache::retrieve(token), None, "global cache must not see it");
}

#[tokio::test]
async fn report_records_applied_step_and_ccr_token() {
    let store = cache::MemoryCcrStore::new(10, 1_000_000);
    let mut rows = Vec::new();
    for i in 0..120 {
        rows.push(format!(
            r#"{{"id":{i},"name":"report_account_{i}","email":"report{i}@ex.com","tier":"bronze"}}"#
        ));
    }
    let original = format!("[{}]", rows.join(","));
    let (res, report) = compress_content_with_store_report(&original, None, &opts(), &store).await;

    assert!(res.applied);
    assert_eq!(report.content_kind, ContentKind::Json);
    assert_eq!(report.original_bytes, original.len());
    assert_eq!(report.compacted_bytes, res.text.len());
    assert_eq!(report.applied_steps.len(), 1);
    assert_eq!(report.applied_steps[0].name, "smartcrusher_rows");
    assert_eq!(
        report.applied_steps[0].compressor,
        Some(CompressorKind::SmartCrusher)
    );
    assert_eq!(
        report
            .bloat_estimate
            .map(|estimate| estimate.reason.as_str()),
        Some("json_rows")
    );
    assert_eq!(report.ccr_tokens, vec![res.ccr_token.unwrap()]);
    assert_eq!(report.skip_reason, None);
}

#[tokio::test]
async fn report_records_redacted_skip_reason() {
    let mut o = opts();
    o.router_enabled = false;
    let (res, report) = compress_content_with_store_report(
        "sensitive raw payload",
        None,
        &o,
        &cache::GlobalCcrStore,
    )
    .await;

    assert!(!res.applied);
    assert_eq!(report.skip_reason, Some(PipelineSkipReason::RouterDisabled));
    assert!(report.bloat_estimate.is_some());
    assert!(report.applied_steps.is_empty());
    assert!(report.ccr_tokens.is_empty());
}

#[tokio::test]
async fn report_records_low_bloat_skip_before_compressor_work() {
    let content = (0..180)
        .map(|i| format!("unique observation {i} has ordinary prose without repeated bulk."))
        .collect::<Vec<_>>()
        .join("\n");
    let hint = ContentHint {
        explicit: Some(ContentKind::PlainText),
        ..Default::default()
    };
    let (res, report) =
        compress_content_with_store_report(&content, Some(hint), &opts(), &cache::GlobalCcrStore)
            .await;

    assert!(!res.applied);
    assert_eq!(res.text, content);
    assert_eq!(report.skip_reason, Some(PipelineSkipReason::LowBloat));
    assert_eq!(
        report
            .bloat_estimate
            .map(|estimate| estimate.reason.as_str()),
        Some("low_signal")
    );
    assert!(report.applied_steps.is_empty());
}

#[tokio::test]
async fn shell_policy_keeps_exact_file_read_raw_even_with_extension_hint() {
    let content = (0..120)
        .map(|i| format!("fn generated_{i}() {{ println!(\"{i}\"); }}"))
        .collect::<Vec<_>>()
        .join("\n");
    let hint = ContentHint {
        source_tool: Some("shell".to_owned()),
        extension: Some("rs".to_owned()),
        ..Default::default()
    };
    let input = CompressInput {
        content: &content,
        kind: ContentKind::PlainText,
        hint: &hint,
        exit_code: None,
        command: Some("cat src/lib.rs".to_owned()),
        argv: None,
        original_bytes: content.len(),
    };

    let store = cache::MemoryCcrStore::new(10, 1_000_000);
    let (res, report) = route_with_store_report(input, &opts(), &store).await;

    assert!(!res.applied);
    assert_eq!(res.text, content);
    assert_eq!(
        report.skip_reason,
        Some(PipelineSkipReason::Other("skip_file_content"))
    );
}

#[tokio::test]
async fn shell_policy_skip_all_keeps_command_output_raw() {
    let content = "line one\nline two\n".repeat(120);
    let hint = ContentHint {
        source_tool: Some("shell".to_owned()),
        ..Default::default()
    };
    let input = CompressInput {
        content: &content,
        kind: ContentKind::PlainText,
        hint: &hint,
        exit_code: None,
        command: Some("rg --files".to_owned()),
        argv: None,
        original_bytes: content.len(),
    };

    let store = cache::MemoryCcrStore::new(10, 1_000_000);
    let (res, report) = route_with_store_report_shell_policy(
        input,
        &opts(),
        &store,
        ShellCompactionPolicy::SkipAll,
    )
    .await;

    assert!(!res.applied);
    assert_eq!(res.text, content);
    assert_eq!(
        report.skip_reason,
        Some(PipelineSkipReason::Other("skip_all"))
    );
}

#[tokio::test]
async fn shell_policy_compact_all_allows_file_content_commands() {
    let mut rows = Vec::new();
    for i in 0..120 {
        rows.push(format!(
            r#"{{"id":{i},"name":"account_{i}","email":"a{i}@ex.com","tier":"gold"}}"#
        ));
    }
    let content = format!("[{}]", rows.join(","));
    let hint = ContentHint {
        source_tool: Some("shell".to_owned()),
        extension: Some("json".to_owned()),
        ..Default::default()
    };
    let input = CompressInput {
        content: &content,
        kind: ContentKind::PlainText,
        hint: &hint,
        exit_code: None,
        command: Some("cat src/lib.rs".to_owned()),
        argv: None,
        original_bytes: content.len(),
    };

    let mut opts = opts();
    opts.ccr_min_tokens = 1;
    let store = cache::MemoryCcrStore::new(10, 1_000_000);
    let (res, report) = route_with_store_report_shell_policy(
        input,
        &opts,
        &store,
        ShellCompactionPolicy::CompactAll,
    )
    .await;

    assert!(res.applied);
    assert_eq!(res.content_kind, ContentKind::Json);
    assert_ne!(res.text, content);
    assert_eq!(report.skip_reason, None);
}

#[tokio::test]
async fn file_read_hint_is_exact_by_default_even_with_code_extension() {
    let content = (0..120)
        .map(|i| format!("pub fn generated_{i}() {{ println!(\"{i}\"); }}"))
        .collect::<Vec<_>>()
        .join("\n");
    let hint = ContentHint {
        source_tool: Some("file_read".to_owned()),
        extension: Some("rs".to_owned()),
        ..Default::default()
    };
    let store = cache::MemoryCcrStore::new(10, 1_000_000);
    let (res, report) =
        compress_content_with_store_report(&content, Some(hint), &opts(), &store).await;

    assert!(!res.applied);
    assert_eq!(res.text, content);
    assert_eq!(
        report.skip_reason,
        Some(PipelineSkipReason::Other("exact_file_read"))
    );
}

#[tokio::test]
async fn lossy_output_declines_when_injected_store_cannot_retain() {
    let store = cache::MemoryCcrStore::new(10, 16);
    let mut rows = Vec::new();
    for i in 0..120 {
        rows.push(format!(
            r#"{{"id":{i},"name":"account_{i}","email":"a{i}@ex.com","tier":"gold"}}"#
        ));
    }
    let original = format!("[{}]", rows.join(","));
    let res = compress_content_with_store(&original, None, &opts(), &store).await;

    assert!(!res.applied);
    assert_eq!(res.text, original);
    assert!(res.ccr_token.is_none());
}

#[tokio::test]
async fn small_input_passes_through() {
    let res = compress_content("tiny", None, &opts()).await;
    assert!(!res.applied);
    assert_eq!(res.text, "tiny");
}

#[tokio::test]
async fn html_hint_extracts_text() {
    let mut html = String::from("<html><body>");
    for i in 0..60 {
        html.push_str(&format!("<div><span>cell number {i} content</span></div>"));
    }
    html.push_str("</body></html>");
    let hint = ContentHint {
        mime: Some("text/html".into()),
        ..Default::default()
    };
    let res = compress_content(&html, Some(hint), &opts()).await;
    assert!(res.applied);
    assert_eq!(res.content_kind, ContentKind::Html);
    assert!(res.text.contains("cell number 7 content"));
}

fn big_json_array() -> String {
    let mut rows = Vec::new();
    for i in 0..120 {
        rows.push(format!(
            r#"{{"id":{i},"name":"account_{i}","email":"a{i}@ex.com","tier":"gold"}}"#
        ));
    }
    format!("[{}]", rows.join(","))
}

/// A log that the signal path *drops* down to its errors — a genuine
/// information-dropping payload (not a faithful reshape).
fn big_log() -> String {
    let mut log = String::new();
    for i in 0..200 {
        if i == 137 {
            log.push_str(&format!(
                "2026-07-05T09:00:00Z ERROR worker request failed: upstream timeout id={i}\n"
            ));
        } else {
            log.push_str(&format!(
                "2026-07-05T09:00:00Z INFO worker handled request {i} in 20ms\n"
            ));
        }
    }
    log
}

fn log_hint() -> ContentHint {
    ContentHint {
        explicit: Some(ContentKind::Log),
        ..Default::default()
    }
}

/// The opt-in escape hatch still works: a host that sets
/// `lossy_without_ccr = true` gets marked-but-unrecoverable lossy output
/// when CCR is off.
#[tokio::test]
async fn lossy_compression_works_with_opt_in_without_ccr() {
    let mut o = opts();
    o.ccr_enabled = false;
    o.lossy_without_ccr = true; // explicit opt-in, not the default
    let original = big_log();
    let res = compress_content(&original, Some(log_hint()), &o).await;
    assert_eq!(res.content_kind, ContentKind::Log);
    assert!(res.applied, "opt-in lossy compression applies without CCR");
    assert!(res.text.len() < original.len());
    assert!(res.ccr_token.is_none(), "no recovery token without CCR");
    assert!(
        !res.text.contains("⟦tj:"),
        "no dangling footer: {}",
        res.text
    );
}

/// The default may still apply lossless reformats when CCR is disabled; it
/// only declines information-dropping lossy output that cannot be recovered.
#[tokio::test]
async fn default_allows_lossless_without_ccr() {
    let mut o = opts();
    o.ccr_enabled = false;
    assert!(!o.lossy_without_ccr, "default is strict");
    let original = big_log();
    let res = compress_content(&original, Some(log_hint()), &o).await;
    assert!(res.applied, "lossless reformat should apply: {}", res.text);
    assert!(!res.lossy, "without CCR output must not drop data");
    assert!(res.ccr_token.is_none());
    assert!(res.text.len() < original.len());
}

/// A large JSON array with CCR off declines lossy row sampling under the
/// strict default, because the sampled-away rows would not be recoverable.
#[tokio::test]
async fn json_declines_lossy_sampling_without_ccr() {
    let mut o = opts();
    o.ccr_enabled = false;
    assert!(!o.lossy_without_ccr, "strict default");
    let original = big_json_array();
    let res = compress_content(&original, None, &o).await;
    assert_eq!(res.content_kind, ContentKind::Json);
    assert!(!res.applied, "unrecoverable lossy view declined: {res:?}");
    assert_eq!(res.text, original);
    assert!(res.ccr_token.is_none());
}

/// A faithful, information-preserving reshape (HTML extraction here) is not
/// lossy, so it ships even with CCR off — reshaping is always allowed, only
/// *dropping* needs a recovery path.
#[tokio::test]
async fn faithful_reshape_ships_without_ccr() {
    let mut o = opts();
    o.ccr_enabled = false;
    assert!(!o.lossy_without_ccr);
    let mut html = String::from("<html><head><title>Status</title></head><body>");
    for i in 0..80 {
        html.push_str(&format!(
            "<div class=\"row\"><span>service {i} is healthy</span></div>"
        ));
    }
    html.push_str("</body></html>");
    let hint = ContentHint {
        mime: Some("text/html".into()),
        explicit: Some(ContentKind::Html),
        ..Default::default()
    };
    let res = compress_content(&html, Some(hint), &o).await;
    assert_eq!(res.content_kind, ContentKind::Html);
    assert!(
        res.applied,
        "faithful reshape ships without CCR: {}",
        res.text
    );
    assert!(!res.lossy, "extraction is information-preserving");
    assert!(res.ccr_token.is_none(), "no recovery needed for a reshape");
    // Every service line's text survives the reshape.
    for i in 0..80 {
        assert!(
            res.text.contains(&format!("service {i} is healthy")),
            "row {i} kept: {}",
            res.text
        );
    }
}

/// A collapsible source file with many long function bodies. Big enough to
/// clear the global floor and to have bodies worth collapsing.
fn big_code_file() -> String {
    let mut src = String::from("use std::collections::HashMap;\n\n");
    for f in 0..4 {
        src.push_str(&format!("pub fn worker_{f}(items: &[i32]) -> i32 {{\n"));
        for i in 0..30 {
            src.push_str(&format!(
                "    let tmp_{f}_{i} = items.iter().sum::<i32>() + {i};\n"
            ));
        }
        src.push_str(&format!("    tmp_{f}_0\n}}\n\n"));
    }
    src
}

fn code_hint() -> ContentHint {
    ContentHint {
        explicit: Some(ContentKind::Code),
        extension: Some("rs".into()),
        ..Default::default()
    }
}

/// Code must never be cut when it can't be recovered: with CCR off and the
/// default options (`lossy_without_ccr = false`), collapsing a function
/// body drops information with no retrieval token, so the router passes the
/// source through verbatim instead.
#[tokio::test]
async fn code_is_not_cut_without_ccr() {
    let mut o = opts();
    o.ccr_enabled = false;
    assert!(
        !o.lossy_without_ccr,
        "default declines info-dropping output without CCR"
    );
    let original = big_code_file();
    let res = compress_content(&original, Some(code_hint()), &o).await;
    assert_eq!(res.content_kind, ContentKind::Code);
    assert!(
        !res.applied,
        "unrecoverable code must pass through, not be cut: {}",
        res.text
    );
    assert_eq!(res.text, original, "source returned verbatim");
    assert!(
        res.text.contains("tmp_1_15"),
        "no body lines eaten: {}",
        res.text
    );
}

/// With CCR on, code still collapses — every omitted body is individually
/// retrievable, so cutting it is safe.
#[tokio::test]
async fn code_collapses_with_ccr() {
    let o = opts();
    assert!(o.ccr_enabled);
    let store = cache::MemoryCcrStore::default();
    let original = big_code_file();
    let res = compress_content_with_store(&original, Some(code_hint()), &o, &store).await;
    assert_eq!(res.content_kind, ContentKind::Code);
    assert!(res.applied, "CCR makes the collapse recoverable");
    assert!(res.text.len() < original.len());
    assert!(res.text.contains("pub fn worker_0"), "signatures kept");
    // Every collapsed body carries a per-block retrieval token.
    let tokens = cache::parse_markers(&res.text);
    assert!(
        !tokens.is_empty(),
        "recoverable tokens present: {}",
        res.text
    );
    let body = store.get(&tokens[0]).expect("stored");
    assert!(body.contains("tmp_0_15"), "full body recoverable: {body}");
}

#[tokio::test]
async fn small_log_above_log_floor_is_compressed() {
    // Default floors: global 2048, log 512. A ~1.8 KB failure log sits
    // between them and must still be compressed.
    let mut log = String::new();
    for i in 0..45 {
        log.push_str(&format!("info sync {i}\n"));
    }
    for i in 0..60 {
        log.push_str(&format!("error shard down {i}\n"));
    }
    assert!(log.len() > 512 && log.len() < 2048, "len={}", log.len());

    let res = compress_content(&log, None, &CompressOptions::default()).await;
    assert!(res.applied, "small log must compress: {res:?}");
    assert_eq!(res.content_kind, ContentKind::Log);
    assert!(res.text.len() < log.len());
}

#[tokio::test]
async fn small_command_output_above_log_floor_runs_rules() {
    // Command output between the log floor and the global floor still
    // reaches the rule engine.
    let mut lines = vec!["On branch main".to_string()];
    for index in 0..30 {
        lines.push(format!("\tmodified:   src/some_module/file_{index}.rs"));
    }
    let content = lines.join("\n");
    assert!(content.len() > 512 && content.len() < 2048);
    let hint = ContentHint::default();
    let input = CompressInput {
        content: &content,
        kind: ContentKind::PlainText,
        hint: &hint,
        exit_code: Some(0),
        command: Some("git status".to_string()),
        argv: Some(vec!["git".to_string(), "status".to_string()]),
        original_bytes: content.len(),
    };
    let res = route(input, &CompressOptions::default()).await;
    assert!(res.applied, "command output must compress: {res:?}");
    assert!(res.text.len() < content.len());
}

#[tokio::test]
async fn small_json_below_global_floor_can_reformat_losslessly() {
    // Typed lossless reformats are allowed before the global lossy floor.
    let mut rows = Vec::new();
    for i in 0..20 {
        rows.push(format!(
            r#"{{"id":{i},"name":"account_{i}","email":"a{i}@ex.com","tier":"gold"}}"#
        ));
    }
    let original = format!("[{}]", rows.join(","));
    assert!(original.len() > 512 && original.len() < 2048);
    let res = compress_content(&original, None, &CompressOptions::default()).await;
    assert!(res.applied, "small JSON can reformat: {res:?}");
    assert!(!res.lossy);
    assert!(res.ccr_token.is_none());
    assert!(res.text.len() < original.len());
}

#[tokio::test]
async fn tiny_log_below_log_floor_passes_through() {
    let log = "error failed to start\nwarning low disk\n".repeat(3);
    assert!(log.len() < 512);
    let res = compress_content(&log, None, &CompressOptions::default()).await;
    assert!(!res.applied);
    assert_eq!(res.text, log);
}

#[tokio::test]
async fn heavy_crush_below_ccr_min_tokens_can_apply_losslessly() {
    // ~470 estimated tokens — under the flat ccr_min_tokens (500) but well
    // over its quarter — crushed to less than half the tokens: the
    // ratio-aware gate must offload so the original stays retrievable.
    let mut o = opts();
    assert_eq!(o.ccr_min_tokens, 500);
    o.min_bytes_to_compress = 64;
    // Long repeated keys, short values: the tabular re-render (keys
    // emitted once) crushes this to well under half the tokens.
    let mut rows = Vec::new();
    for i in 0..18 {
        rows.push(format!(
            r#"{{"identifier":{i},"account_name":"a{i}","email_address":"a{i}@x.io","subscription_tier":"g","current_status":"ok"}}"#
        ));
    }
    let original = format!("[{}]", rows.join(","));
    let original_tokens = crate::tokens::estimate_tokens(&original) as usize;
    assert!(
        (125..500).contains(&original_tokens),
        "tokens={original_tokens}"
    );
    let res = compress_content(&original, None, &o).await;
    assert!(res.applied, "response: {res:?}");
    if res.lossy {
        let token = res.ccr_token.expect("lossy output must offload");
        assert_eq!(cache::retrieve(&token).as_deref(), Some(original.as_str()));
    } else {
        assert!(
            res.ccr_token.is_none(),
            "lossless output should not require recovery"
        );
    }
}

#[tokio::test]
async fn router_disabled_is_passthrough() {
    let mut o = opts();
    o.router_enabled = false;
    let big = "x".repeat(5000);
    let res = compress_content(&big, None, &o).await;
    assert!(!res.applied);
    assert_eq!(res.text, big);
}

#[tokio::test]
async fn policy_wrappers_preserve_output_when_shell_compaction_is_disabled() {
    let hint = ContentHint::default();
    let content = big_log();
    let make_input = || CompressInput {
        content: &content,
        kind: ContentKind::Log,
        hint: &hint,
        exit_code: Some(0),
        command: Some("custom command".into()),
        argv: None,
        original_bytes: content.len(),
    };
    let store = cache::MemoryCcrStore::default();
    let out = route_with_store_shell_policy(
        make_input(),
        &opts(),
        &store,
        ShellCompactionPolicy::SkipAll,
    )
    .await;
    assert_eq!(out.text, content);
    assert!(!out.applied);
    let out = route_with_shell_policy(make_input(), &opts(), ShellCompactionPolicy::SkipAll).await;
    assert_eq!(out.text, content);
    assert!(!out.applied);
}

#[tokio::test]
async fn content_gates_disable_search_code_and_html_without_command_fallback() {
    let store = cache::MemoryCcrStore::default();
    for kind in [ContentKind::Search, ContentKind::Code, ContentKind::Html] {
        let content = "ordinary payload without reducible structure ".repeat(40);
        let hint = ContentHint {
            explicit: Some(kind),
            query: Some("force routing".into()),
            ..Default::default()
        };
        let options = CompressOptions {
            search_enabled: false,
            code_enabled: false,
            html_enabled: false,
            ..opts()
        };
        let (out, report) =
            compress_content_with_store_report(&content, Some(hint), &options, &store).await;
        assert_eq!(out.text, content);
        assert!(!out.applied);
        assert_eq!(report.skip_reason, Some(PipelineSkipReason::NoCompressor));
    }
}

#[test]
fn detect_only_respects_hard_hint_without_transforming_payload() {
    assert_eq!(
        detect_only(
            "payload",
            &ContentHint {
                explicit: Some(ContentKind::Html),
                ..Default::default()
            }
        ),
        ContentKind::Html
    );
}

#[tokio::test]
async fn typed_code_stub_retains_original_and_exposes_structural_preview() {
    let store = cache::MemoryCcrStore::default();
    let content = big_code_file();
    let hint = ContentHint {
        explicit: Some(ContentKind::Code),
        extension: Some("rs".into()),
        read_intent: crate::types::ReadIntent::Stub(crate::types::StubMode::SignaturesOnly),
        ..Default::default()
    };
    let input = CompressInput {
        content: &content,
        kind: ContentKind::Code,
        hint: &hint,
        command: None,
        argv: None,
        exit_code: None,
        original_bytes: content.len(),
    };
    let options = CompressOptions {
        ccr_min_tokens: 0,
        ..opts()
    };
    let out = route_with_store(input, &options, &store).await;
    assert!(out.applied && out.lossy, "{out:?}");
    assert_eq!(out.compressor, CompressorKind::Code);
    assert!(out.body.contains("pub fn worker_0"));
    assert_eq!(
        store.get(out.ccr_token.as_deref().unwrap()).as_deref(),
        Some(content.as_str())
    );
}
