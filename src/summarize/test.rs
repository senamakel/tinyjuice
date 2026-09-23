use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::llm::{self, GenerateRequest};

fn opts() -> CompressOptions {
    CompressOptions {
        llm_summary_enabled: true,
        llm_summary_threshold_tokens: 10,
        llm_summary_max_input_tokens: 10_000,
        ..CompressOptions::default()
    }
}

/// A payload unique to each test, so the process-wide cache never answers
/// for a call another test made.
fn payload(tag: &str) -> String {
    format!("{tag}: ") + &"alpha beta gamma delta ".repeat(40)
}

fn input<'a>(content: &'a str, focus: Option<&'a str>, scope: &'a str) -> SummaryInput<'a> {
    SummaryInput {
        tool_name: "web_fetch",
        content,
        focus,
        context_token: Some("turn-1"),
        scope: Some(scope),
    }
}

/// Install a callback that records every request and answers `reply`.
fn recording(reply: Result<Option<String>, String>) -> Arc<Mutex<Vec<GenerateRequest>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    llm::configure_callback(Some(Arc::new(move |request: GenerateRequest| {
        sink.lock().unwrap().push(request);
        let reply = reply.clone();
        Box::pin(async move { reply })
    })));
    seen
}

#[tokio::test]
async fn below_threshold_is_not_needed_and_makes_no_call() {
    let _guard = llm::callback_test_guard().await;
    let seen = recording(Ok(Some("short".into())));
    let outcome = maybe_summarize(input("tiny", None, "below"), &opts()).await;
    assert_eq!(outcome, SummaryOutcome::NotNeeded);
    assert!(seen.lock().unwrap().is_empty());
    llm::configure_callback(None);
}

#[tokio::test]
async fn disabled_or_tokenless_calls_are_not_needed() {
    let _guard = llm::callback_test_guard().await;
    let seen = recording(Ok(Some("short".into())));
    let raw = payload("disabled");
    let off = CompressOptions {
        llm_summary_enabled: false,
        ..opts()
    };
    assert_eq!(
        maybe_summarize(input(&raw, None, "disabled"), &off).await,
        SummaryOutcome::NotNeeded
    );
    let tokenless = SummaryInput {
        context_token: None,
        ..input(&raw, None, "disabled")
    };
    assert_eq!(
        maybe_summarize(tokenless, &opts()).await,
        SummaryOutcome::NotNeeded
    );
    assert!(seen.lock().unwrap().is_empty());
    llm::configure_callback(None);
}

#[tokio::test]
async fn above_the_input_cap_is_disclosed_as_unavailable() {
    let _guard = llm::callback_test_guard().await;
    recording(Ok(Some("short".into())));
    let raw = payload("too-large");
    let small_cap = CompressOptions {
        llm_summary_max_input_tokens: 20,
        ..opts()
    };
    assert_eq!(
        maybe_summarize(input(&raw, None, "too-large"), &small_cap).await,
        SummaryOutcome::Unavailable(UnavailableReason::PayloadTooLarge)
    );
    llm::configure_callback(None);
}

#[tokio::test]
async fn a_summary_replaces_the_payload_and_keeps_the_original_recoverable() {
    let _guard = llm::callback_test_guard().await;
    let seen = recording(Ok(Some("  the gist  ".into())));
    let raw = payload("summarized");
    let outcome = maybe_summarize(
        input(&raw, Some("the install steps"), "summarized"),
        &opts(),
    )
    .await;
    let SummaryOutcome::Summarized {
        text,
        original_bytes,
        summary_bytes,
        ccr_token,
    } = outcome
    else {
        panic!("expected a summary, got {outcome:?}");
    };
    assert!(text.starts_with("the gist"));
    assert_eq!(original_bytes, raw.len());
    assert_eq!(summary_bytes, "the gist".len());
    let token = ccr_token.expect("the original should be offloaded");
    assert!(text.contains(&token), "the footer names the token");
    assert_eq!(
        crate::cache::retrieve(&token).as_deref(),
        Some(raw.as_str())
    );

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].context_token, "turn-1");
    assert_eq!(requests[0].purpose, PURPOSE);
    assert_eq!(requests[0].system, SYSTEM_PROMPT);
    assert!(
        requests[0]
            .prompt
            .contains("Caller focus: the install steps")
    );
    drop(requests);
    llm::configure_callback(None);
}

#[tokio::test]
async fn a_host_that_declines_is_not_a_failure() {
    let _guard = llm::callback_test_guard().await;
    recording(Ok(None));
    let raw = payload("declined");
    assert_eq!(
        maybe_summarize(input(&raw, None, "declined"), &opts()).await,
        SummaryOutcome::NotNeeded
    );
    llm::configure_callback(None);
}

#[tokio::test]
async fn empty_or_non_shrinking_replies_fail() {
    let _guard = llm::callback_test_guard().await;
    let raw = payload("non-shrinking");
    recording(Ok(Some("   ".into())));
    assert_eq!(
        maybe_summarize(input(&raw, None, "non-shrinking"), &opts()).await,
        SummaryOutcome::Unavailable(UnavailableReason::Failed)
    );
    recording(Ok(Some(raw.clone() + " and more")));
    assert_eq!(
        maybe_summarize(input(&raw, None, "non-shrinking"), &opts()).await,
        SummaryOutcome::Unavailable(UnavailableReason::Failed)
    );
    llm::configure_callback(None);
}

#[tokio::test]
async fn three_failures_open_the_breaker_for_that_scope_only() {
    let _guard = llm::callback_test_guard().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    llm::configure_callback(Some(Arc::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err("model offline".to_string()) })
    })));
    for round in 0..MAX_CONSECUTIVE_FAILURES {
        let raw = payload(&format!("breaker-{round}"));
        assert_eq!(
            maybe_summarize(input(&raw, None, "breaker"), &opts()).await,
            SummaryOutcome::Unavailable(UnavailableReason::Failed)
        );
    }
    let raw = payload("breaker-after");
    assert_eq!(
        maybe_summarize(input(&raw, None, "breaker"), &opts()).await,
        SummaryOutcome::Unavailable(UnavailableReason::Disabled)
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        MAX_CONSECUTIVE_FAILURES as usize
    );

    // Another conversation is unaffected.
    assert_eq!(
        maybe_summarize(input(&raw, None, "breaker-other"), &opts()).await,
        SummaryOutcome::Unavailable(UnavailableReason::Failed)
    );
    llm::configure_callback(None);
}

#[tokio::test]
async fn an_identical_payload_reuses_its_summary_but_another_focus_does_not() {
    let _guard = llm::callback_test_guard().await;
    let seen = recording(Ok(Some("the gist".into())));
    let raw = payload("cache");
    for _ in 0..2 {
        let outcome = maybe_summarize(input(&raw, Some("pricing"), "cache"), &opts()).await;
        assert!(matches!(outcome, SummaryOutcome::Summarized { .. }));
    }
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the repeat is served from cache"
    );

    let outcome = maybe_summarize(input(&raw, Some("the changelog"), "cache"), &opts()).await;
    assert!(matches!(outcome, SummaryOutcome::Summarized { .. }));
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "a new focus is a new summary"
    );
    llm::configure_callback(None);
}

#[test]
fn the_prompt_states_tool_focus_and_exact_size() {
    let prompt = build_prompt("web_fetch", Some("  auth flow  "), "payload");
    assert!(prompt.starts_with("Tool name: web_fetch\n\nCaller focus: auth flow\n\n"));
    assert!(prompt.contains("Raw tool output: 7 bytes, complete"));
    assert!(prompt.contains("--- BEGIN ---\npayload\n--- END ---"));

    let without = build_prompt("web_fetch", Some("   "), "payload");
    assert!(!without.contains("Caller focus"));
}

#[test]
fn a_long_focus_keeps_its_trailing_request() {
    let focus = "x".repeat(FOCUS_MAX_CHARS * 2) + " find the timeout setting";
    let clipped = clip_focus(&focus);
    assert!(clipped.chars().count() < focus.chars().count());
    assert!(clipped.ends_with("find the timeout setting"));
    assert!(clipped.contains("characters omitted"));
}

#[test]
fn the_contract_tells_the_model_to_extract_for_the_focus() {
    assert!(SYSTEM_PROMPT.contains("caller focus"));
    assert!(SYSTEM_PROMPT.contains("Do not answer the focus"));
}
