//! End-to-end proof that the built TinyJuice cdylib is admitted and serves
//! compression and CCR retrieval through a real TinyBus broker.

use std::sync::Arc;
use std::time::Duration;

use tinybus::Connection;
use tinybus::module::ModuleState;
use tinybus::test_support::{admit_module, start_bus, wait_until_serving};
use tinyjuice::types::{AgentTokenjuiceCompression, CompressedOutput, ContentHint, ContentKind};
use tinyjuice_bus::wire::CompactResponse;
use tinyjuice_module::{BUS_NAME, OBJECT_PATH};
use tinystoragedrivers_api::{BlobStore, StorageBackend};
use tinystoragedrivers_busproxy::{PerCallerEndpoint, StorageServer, serve};
use tinystoragedrivers_testkit::MemoryBackend;
use tinystoragedrivers_types::Scope;

const EXPECTED_METHODS: &[&str] = tinyjuice_bus::METHODS;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires TINYJUICE_TEST_MODULE to point at the built cdylib"]
async fn the_built_module_compresses_and_recovers_over_a_real_broker() {
    let (modules, client, broker_task) = start_bus().await.expect("memory bus should start");
    let storage_backend = Arc::new(MemoryBackend::new());
    let endpoint_backend = Arc::clone(&storage_backend);
    let storage_endpoint = PerCallerEndpoint::new(move |_sender| {
        let scope = Scope::new("tinyjuice-module-test").ok()?;
        let storage = endpoint_backend.for_scope(&scope).ok()?;
        Some(StorageServer::new(storage))
    });
    let _served_storage = serve(&client, Arc::new(storage_endpoint))
        .await
        .expect("host storage should be served before module setup");
    let loaded = admit_module(&modules, "TINYJUICE_TEST_MODULE", "tinyjuice-module")
        .expect("module should load");
    assert_eq!(loaded.name, "tinyjuice-module");
    assert_eq!(loaded.manifest.bus_name.as_str(), BUS_NAME);
    assert_eq!(loaded.manifest.object_path.as_str(), OBJECT_PATH);
    let methods: Vec<&str> = loaded
        .manifest
        .provides
        .iter()
        .flat_map(|interface| interface.methods.iter())
        .map(tinybus::MemberName::as_str)
        .collect();
    assert_eq!(methods, EXPECTED_METHODS);

    wait_until_serving(&client, BUS_NAME, Duration::from_secs(5))
        .await
        .expect("module should become ready");
    let proxy = client
        .proxy(BUS_NAME, OBJECT_PATH, BUS_NAME)
        .expect("module proxy");

    proxy
        .call::<()>(
            "Install",
            (serde_json::json!({
                "options": {
                    "routerEnabled": true,
                    "ccrEnabled": true,
                    "searchEnabled": true,
                    "codeEnabled": true,
                    "htmlEnabled": true,
                    "mlTextEnabled": false,
                    "minBytesToCompress": 64,
                    "minBytesToCompressLog": 32,
                    "ccrMinTokens": 16,
                    "lossyWithoutCcr": false,
                    "maxInlineChars": null,
                    "codeTargetRatio": null,
                    "charsPerToken": 4.0
                },
                "maxCacheEntries": 1,
                "maxCacheBytes": 1048576,
                "ccrTtlSecs": null,
                "ccrDurable": true
            }),),
        )
        .await
        .expect("install should succeed");

    let detected: String = proxy
        .call(
            "Detect",
            ("ordinary content".to_string(), ContentHint::default()),
        )
        .await
        .expect("detect should succeed");
    assert_eq!(detected, ContentKind::PlainText.as_str());
    assert!(
        proxy.call::<String>("Detect", ()).await.is_err(),
        "detect should reject malformed arguments"
    );

    let compact_content = "compact is disabled for this call";
    let compacted: CompactResponse = proxy
        .call(
            "Compact",
            (
                compact_content.to_string(),
                "web_fetch".to_string(),
                false,
                AgentTokenjuiceCompression::Off,
            ),
        )
        .await
        .expect("compact should return a response when compaction is disabled");
    assert!(!compacted.applied);
    assert_eq!(compacted.text, compact_content);
    assert!(
        proxy.call::<CompactResponse>("Compact", ()).await.is_err(),
        "compact should reject malformed arguments"
    );

    let content = (0..200)
        .map(|index| {
            if index == 137 {
                format!("ERROR request failed id={index}\n")
            } else {
                format!("INFO request completed id={index}\n")
            }
        })
        .collect::<String>();
    let output: CompressedOutput = proxy
        .call(
            "Compress",
            (content.clone(), serde_json::json!({ "explicit": "log" })),
        )
        .await
        .expect("compress should succeed");
    assert!(output.applied);
    let token = output.ccr_token.expect("compression should be recoverable");

    let scope = Scope::new("tinyjuice-module-test").expect("valid test scope");
    let host_blobs: Arc<dyn BlobStore> =
        Arc::clone(storage_backend.for_scope(&scope).unwrap().blobs());
    let durable_key = format!("{}/{}", tinyjuice_drivers::CCR_PREFIX, token);
    let durable_blob = host_blobs
        .get(&durable_key)
        .await
        .expect("host blob lookup should succeed")
        .expect("the module should write its CCR original to host storage");
    assert_eq!(durable_blob.bytes, content.as_bytes());

    // A second distinct retained original evicts the first from the one-entry
    // module cache. Retrieval below must therefore use the host BlobStore.
    let eviction_content = (0..200)
        .map(|index| format!("WARN independent request queued id={index}\n"))
        .collect::<String>();
    let eviction_output: CompressedOutput = proxy
        .call(
            "Compress",
            (eviction_content, serde_json::json!({ "explicit": "log" })),
        )
        .await
        .expect("second compression should succeed");
    assert!(eviction_output.applied);
    assert!(eviction_output.ccr_token.is_some());

    let recovered: Option<String> = proxy
        .call(
            "Retrieve",
            (token.clone(), Option::<serde_json::Value>::None),
        )
        .await
        .expect("retrieve should succeed");
    assert_eq!(recovered, Some(content));
    typed_queries_execute_in_the_loaded_artifact(&proxy, &token).await;

    assert!(matches!(modules.list()[0].state, ModuleState::Ready));

    compact_with_calls_back_to_the_host_for_a_focused_summary(&client, &proxy).await;
    savings_are_computed_and_persisted_inside_the_loaded_module(&proxy).await;
    broker_task.abort();
}

/// The host half of the summary stage, as a test host serves it: answers
/// `MlHost.Generate` with a fixed note and remembers the prompt it was sent.
#[derive(Clone)]
struct TestHost {
    prompts: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[tinybus::interface(name = "ai.tinyhumans.tinyjuice.MlHost")]
impl TestHost {
    async fn generate(
        &self,
        request: tinyjuice_bus::wire::GenerateRequest,
    ) -> tinybus::Result<Option<String>> {
        self.prompts.lock().unwrap().push(request.prompt);
        Ok(Some("the limit is 60 requests a minute".to_string()))
    }
}

/// `CompactWith` over the same broker: the module asks the host's
/// `MlHost.Generate` for a summary written for the caller's focus. Runs inside
/// the one test above, because a process loads the cdylib once.
async fn compact_with_calls_back_to_the_host_for_a_focused_summary(
    client: &Connection,
    proxy: &tinybus::Proxy,
) {
    let host = TestHost {
        prompts: Default::default(),
    };
    client
        .serve_at(
            tinyjuice_bus::ML_HOST_PATH.try_into().expect("host path"),
            host.clone(),
        )
        .await
        .expect("host should serve");
    client
        .request_name(tinyjuice_bus::ML_HOST_NAME)
        .await
        .expect("host name");
    proxy
        .call::<()>(
            "Install",
            (serde_json::json!({
                "options": {
                    "ccrEnabled": true,
                    "llmSummaryEnabled": true,
                    "llmSummaryMode": "auto",
                    "llmSummaryThresholdTokens": 10
                },
                "maxCacheEntries": 8,
                "maxCacheBytes": 1048576,
                "ccrTtlSecs": null,
                "ccrDurable": false
            }),),
        )
        .await
        .expect("install should succeed");

    let content = "Requests are rate limited per key. ".repeat(40);
    let response: tinyjuice_bus::wire::CompactResponse = proxy
        .call(
            "CompactWith",
            (serde_json::json!({
                "content": content,
                "toolName": "web_fetch",
                "enabled": false,
                "focus": "the rate limits",
                "contextToken": "turn-1",
                "scope": "e2e"
            }),),
        )
        .await
        .expect("CompactWith should succeed");

    assert_eq!(response.compressor, "llm_summary");
    assert!(
        response
            .text
            .starts_with("the limit is 60 requests a minute")
    );
    assert!(response.text.contains("juice_retrieve"));
    assert!(response.notice.is_none());
    let prompts = host.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("Caller focus: the rate limits"));
    let query = tinyjuice_bus::wire::QueryRequest {
        target: tinyjuice_bus::wire::QueryTarget::Content {
            content: "Requests are rate limited per key. ".repeat(40),
        },
        op: serde_json::from_value(
            serde_json::json!({"op":"summarize","hint":"rate limits for this artifact"}),
        )
        .unwrap(),
        limits: Default::default(),
        context_token: Some("query-turn".into()),
        scope: Some("query-e2e".into()),
    };
    let reply: tinyjuice_bus::wire::QueryResponse = proxy.call("Query", (query,)).await.unwrap();
    assert!(
        matches!(reply, Ok(tinyjuice_bus::repl::ReplOutput::Text { text }) if text == "the limit is 60 requests a minute")
    );
    let prompts = host.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("rate limits for this artifact"));
}

/// Every new member is called through the separately loaded artifact.
async fn typed_queries_execute_in_the_loaded_artifact(proxy: &tinybus::Proxy, token: &str) {
    use tinyjuice_bus::repl::{ReplLimits, ReplOp, ReplOutput};
    use tinyjuice_bus::wire::{QueryError, QueryRequest, QueryResponse, QueryTarget};
    let op: ReplOp =
        serde_json::from_value(serde_json::json!({"op": "find", "query": "ERROR"})).unwrap();
    let request = |target| QueryRequest {
        target,
        op: op.clone(),
        limits: ReplLimits::default(),
        context_token: None,
        scope: None,
    };
    let reply: QueryResponse = proxy
        .call(
            "Query",
            (request(QueryTarget::Handle {
                token: token.into(),
            }),),
        )
        .await
        .unwrap();
    let Ok(ReplOutput::Lines { hits, .. }) = reply else {
        panic!("expected stored log hits")
    };
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].line, 138);
    let reply: QueryResponse = proxy
        .call(
            "Query",
            (request(QueryTarget::Content {
                content: "first\nERROR artifact\nlast".into(),
            }),),
        )
        .await
        .unwrap();
    let Ok(ReplOutput::Lines { hits, .. }) = reply else {
        panic!("expected artifact hits")
    };
    assert_eq!(hits[0].line, 2);
    assert_eq!(hits[0].text, "ERROR artifact");
    let gone: QueryResponse = proxy
        .call(
            "Query",
            (request(QueryTarget::Handle {
                token: "not-cached".into(),
            }),),
        )
        .await
        .unwrap();
    assert_eq!(gone, Err(QueryError::HandleNotFound));
    let before: tinyjuice_bus::wire::CacheStats = proxy.call("CacheStats", ()).await.unwrap();
    let mut capped = request(QueryTarget::Content {
        content: "ERROR one\nERROR two\nERROR three".into(),
    });
    capped.limits.max_hits = 1;
    let reply: QueryResponse = proxy.call("Query", (capped,)).await.unwrap();
    let Ok(ReplOutput::Lines { hits, truncated }) = reply else {
        panic!("expected capped lines")
    };
    assert_eq!(hits.len(), 1);
    assert_eq!(truncated, 2);
    let after: tinyjuice_bus::wire::CacheStats = proxy.call("CacheStats", ()).await.unwrap();
    assert_eq!(
        before.entries, after.entries,
        "supplied content must not create CCR entries"
    );
    assert_eq!(before.bytes, after.bytes);
    for (op, expected) in [(
        serde_json::json!({"op":"find","query":""}),
        QueryError::EmptyQuery,
    )] {
        let mut invalid = request(QueryTarget::Content {
            content: "artifact".into(),
        });
        invalid.op = serde_json::from_value(op).unwrap();
        let reply: QueryResponse = proxy.call("Query", (invalid,)).await.unwrap();
        assert_eq!(reply, Err(expected));
    }
    let mut invalid = request(QueryTarget::Content {
        content: "artifact".into(),
    });
    invalid.op =
        serde_json::from_value(serde_json::json!({"op":"find","mode":"regex","query":"["}))
            .unwrap();
    let reply: QueryResponse = proxy.call("Query", (invalid,)).await.unwrap();
    assert!(matches!(reply, Err(QueryError::InvalidPattern(_))));
    let mut unlimited = request(QueryTarget::Content {
        content: "ERROR bounded\n".repeat(100),
    });
    unlimited.limits.max_hits = usize::MAX;
    unlimited.limits.max_lines = usize::MAX;
    unlimited.limits.max_line_chars = usize::MAX;
    unlimited.limits.max_output_chars = usize::MAX;
    unlimited.limits.regex_size_limit = usize::MAX;
    let reply: QueryResponse = proxy.call("Query", (unlimited,)).await.unwrap();
    let Ok(ReplOutput::Lines { hits, truncated }) = reply else {
        panic!("expected bounded hits")
    };
    assert_eq!(hits.len(), 50);
    assert_eq!(truncated, 50);
    let mut context_query = request(QueryTarget::Content {
        content: "before\n".repeat(10) + "ERROR needle\n" + &"after\n".repeat(10),
    });
    context_query.op = serde_json::from_value(serde_json::json!({
        "op":"find", "mode":"grep", "query":"ERROR", "context":usize::MAX
    }))
    .unwrap();
    context_query.limits.max_lines = 3;
    let context_reply: QueryResponse = proxy.call("Query", (context_query,)).await.unwrap();
    let Ok(ReplOutput::Lines { hits, truncated }) = context_reply else {
        panic!("expected bounded contextual hits")
    };
    assert_eq!(hits.len(), 3);
    assert!(
        hits.iter()
            .any(|hit| hit.line == 11 && hit.text == "ERROR needle")
    );
    assert!(truncated > 0);
    let oversized_query = request(QueryTarget::Content {
        content: "x".repeat(tinyjuice_bus::wire::MAX_QUERY_CONTENT_BYTES + 1),
    });
    let oversized_reply: QueryResponse = proxy.call("Query", (oversized_query,)).await.unwrap();
    assert_eq!(oversized_reply, Err(QueryError::InputTooLarge));
    let oversized: tinyjuice_bus::wire::HtmlResponse = proxy
        .call(
            "ExtractHtml",
            ("x".repeat(tinyjuice_bus::wire::MAX_HTML_INPUT_BYTES + 1),),
        )
        .await
        .unwrap();
    assert_eq!(
        oversized,
        Err(tinyjuice_bus::wire::HtmlError::InputTooLarge)
    );
    let legacy: String = proxy
        .call("Repl", (token, r#"{"op":"find","query":"ERROR"}"#))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<ReplOutput>(&legacy).unwrap(),
        ReplOutput::Lines {
            hits: vec![tinyjuice_bus::repl::Hit {
                line: 138,
                text: "ERROR request failed id=137".into()
            }],
            truncated: 0
        }
    );
    let markdown: tinyjuice_bus::wire::HtmlResponse = proxy.call("ExtractHtml", ("<html><body><h1>Heading</h1><p>Read <a href=\"/details\">details</a></p><script>secret()</script></body></html>",)).await.unwrap();
    let markdown = markdown.expect("bounded HTML extraction");
    assert!(markdown.contains("# Heading"));
    assert!(markdown.contains("[details](/details)"));
    assert!(!markdown.contains("secret()"));
}

async fn savings_are_computed_and_persisted_inside_the_loaded_module(proxy: &tinybus::Proxy) {
    use tinyjuice_bus::names::methods;
    use tinyjuice_bus::savings::{RecordSavingsRequest, SavingsAggregate};
    proxy
        .call::<()>(methods::CONFIGURE_SAVINGS, ("ignored",))
        .await
        .unwrap();
    proxy.call::<()>(methods::RESET_SAVINGS, ()).await.unwrap();
    proxy
        .call::<()>(
            methods::RECORD_SAVINGS,
            (RecordSavingsRequest {
                model: "test-model".into(),
                compressor: "generic".into(),
                original_tokens: 2_000_000,
                compacted_tokens: 1_000_000,
                input_per_mtok_usd: Some(3.0),
            },),
        )
        .await
        .unwrap();
    let stats: SavingsAggregate = proxy.call(methods::SAVINGS_STATS, ()).await.unwrap();
    assert_eq!(stats.total.tokens_saved, 1_000_000);
    assert_eq!(stats.by_model["test-model"].cost_saved_usd, 3.0);
    assert_eq!(stats.by_compressor["generic"].events, 1);
    proxy.call::<()>(methods::RESET_SAVINGS, ()).await.unwrap();
    let stats: SavingsAggregate = proxy.call(methods::SAVINGS_STATS, ()).await.unwrap();
    assert_eq!(stats.total.events, 0);
}
