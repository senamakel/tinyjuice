use super::*;
use tinyjuice::types::CompressOptions;

#[test]
fn setup_config_accepts_legacy_empty_forms_and_a_typed_storage_descriptor() {
    assert!(
        serde_json::from_value::<ModuleSetupConfig>(serde_json::Value::Null)
            .unwrap()
            .storage_descriptor()
            .is_none()
    );
    assert!(
        serde_json::from_value::<ModuleSetupConfig>(serde_json::json!({}))
            .unwrap()
            .storage_descriptor()
            .is_none()
    );
    let configured = serde_json::from_value::<ModuleSetupConfig>(serde_json::json!({
        "storage_descriptor": {
            "service_peer": ":1.23",
            "registration_id": 4,
            "object_root": "/org/tinystoragedrivers/Storage/Leases/4"
        }
    }))
    .unwrap()
    .storage_descriptor()
    .unwrap();
    assert_eq!(configured.service_peer(), ":1.23");
    assert_eq!(
        configured.object_root(),
        "/org/tinystoragedrivers/Storage/Leases/4"
    );
    assert!(
        serde_json::from_value::<ModuleSetupConfig>(serde_json::json!({
            "storage_descriptor": { "unknown": true }
        }))
        .is_err()
    );
}

#[tokio::test]
async fn service_compresses_and_retrieves_a_large_log() {
    let service = Compression::default();
    service
        .install(InstallRequest {
            options: CompressOptions {
                min_bytes_to_compress: 64,
                ccr_min_tokens: 16,
                ..CompressOptions::default()
            },
            max_cache_entries: 8,
            max_cache_bytes: 1024 * 1024,
            ccr_ttl_secs: None,
            ccr_durable: false,
        })
        .await
        .expect("install should succeed");

    let content = (0..200)
        .map(|index| {
            if index == 137 {
                format!("ERROR request failed id={index}\n")
            } else {
                format!("INFO request completed id={index}\n")
            }
        })
        .collect::<String>();
    let output = service
        .compress(
            content.clone(),
            ContentHint {
                explicit: Some(tinyjuice::types::ContentKind::Log),
                ..ContentHint::default()
            },
        )
        .await
        .expect("compression should succeed");
    assert!(output.applied);
    let token = output
        .ccr_token
        .expect("lossy compression should be recoverable");
    assert_eq!(
        service
            .retrieve(token, None)
            .await
            .expect("retrieve should succeed"),
        Some(content)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compression_services_keep_durable_ccr_in_their_own_storage() {
    use crate::storage::tests::rig;
    use tinystoragedrivers_core::{Scope, StorageBackend};

    let first = rig(true).await.unwrap();
    let second = rig(true).await.unwrap();
    let first_storage = crate::storage::bind(first.module.clone(), None)
        .await
        .unwrap();
    let second_storage = crate::storage::bind(second.module.clone(), None)
        .await
        .unwrap();
    let first_service = Compression::new(Some(first.module.clone()), first_storage, false);
    let second_service = Compression::new(Some(second.module.clone()), second_storage, false);
    let install = || InstallRequest {
        options: CompressOptions {
            min_bytes_to_compress: 64,
            ccr_min_tokens: 16,
            ..CompressOptions::default()
        },
        // Force this test's original to rely on its selected durable store.
        max_cache_entries: 1,
        max_cache_bytes: 1,
        ccr_ttl_secs: Some(600),
        ccr_durable: true,
    };
    first_service.install(install()).await.unwrap();
    second_service.install(install()).await.unwrap();

    let content = (0..200)
        .map(|index| {
            if index == 137 {
                format!("ERROR request failed id={index}\n")
            } else {
                format!("INFO request completed id={index}\n")
            }
        })
        .collect::<String>();
    let result = first_service
        .compress(
            content.clone(),
            ContentHint {
                explicit: Some(tinyjuice::types::ContentKind::Log),
                ..ContentHint::default()
            },
        )
        .await
        .unwrap();
    let token = result
        .ccr_token
        .expect("durable storage should retain the original");
    let key = format!("{}{}", tinyjuice_bus::STORAGE_NEEDS.blob_prefixes[0], token);

    let first_storage = first.backend.for_scope(&Scope::local()).unwrap();
    let second_storage = second.backend.for_scope(&Scope::local()).unwrap();
    let first_blobs = first_storage.blobs();
    let second_blobs = second_storage.blobs();
    let first_has_blob = first_blobs.get(&key).await.unwrap().is_some();
    let second_has_blob = second_blobs.get(&key).await.unwrap().is_some();
    assert!(
        first_has_blob,
        "the first service must write the original to its own storage; first={first_has_blob}, second={second_has_blob}"
    );
    assert!(
        !second_has_blob,
        "a second service must not receive another instance's CCR original"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compression_services_keep_savings_in_their_own_documents() {
    use crate::storage::tests::rig;
    use tinyjuice_bus::savings::RecordSavingsRequest;
    use tinystoragedrivers_core::{Scope, StorageBackend};

    let first = rig(true).await.unwrap();
    let second = rig(true).await.unwrap();
    let first_storage = crate::storage::bind(first.module.clone(), None)
        .await
        .unwrap()
        .unwrap();
    let second_storage = crate::storage::bind(second.module.clone(), None)
        .await
        .unwrap()
        .unwrap();
    let first_service = Compression::new(Some(first.module.clone()), Some(first_storage), false);
    let second_service = Compression::new(Some(second.module.clone()), Some(second_storage), false);

    first_service
        .record_savings(RecordSavingsRequest {
            model: "profile-a".into(),
            compressor: "generic".into(),
            original_tokens: 200,
            compacted_tokens: 100,
            input_per_mtok_usd: None,
        })
        .await
        .unwrap();
    second_service
        .record_savings(RecordSavingsRequest {
            model: "profile-b".into(),
            compressor: "generic".into(),
            original_tokens: 500,
            compacted_tokens: 200,
            input_per_mtok_usd: None,
        })
        .await
        .unwrap();

    let first_stats = first_service.savings_stats().await.unwrap();
    let second_stats = second_service.savings_stats().await.unwrap();
    assert_eq!(first_stats.total.tokens_saved, 100);
    assert_eq!(second_stats.total.tokens_saved, 300);
    assert!(first_stats.by_model.contains_key("profile-a"));
    assert!(!first_stats.by_model.contains_key("profile-b"));
    assert!(second_stats.by_model.contains_key("profile-b"));
    assert!(!second_stats.by_model.contains_key("profile-a"));

    let first_doc = first
        .backend
        .for_scope(&Scope::local())
        .unwrap()
        .documents()
        .get(
            tinyjuice_bus::storage::SAVINGS_COLLECTION,
            tinyjuice_bus::savings::SAVINGS_DOC_ID,
        )
        .await
        .unwrap()
        .unwrap();
    let second_doc = second
        .backend
        .for_scope(&Scope::local())
        .unwrap()
        .documents()
        .get(
            tinyjuice_bus::storage::SAVINGS_COLLECTION,
            tinyjuice_bus::savings::SAVINGS_DOC_ID,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_doc.doc["total"]["tokens_saved"], 100);
    assert_eq!(second_doc.doc["total"]["tokens_saved"], 300);
}

#[tokio::test]
async fn service_repl_greps_a_stored_output_and_reports_bad_input() {
    let service = Compression::default();
    use tinyjuice::cache::CcrStore;
    let token = service
        .state
        .ccr
        .put("alpha\nbeta needle\ngamma\n")
        .token()
        .to_string();
    let reply = service
        .repl(token.clone(), r#"{"op":"find","query":"needle"}"#.into())
        .await
        .expect("repl should reply");
    let value: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(value["hits"][0]["line"], 2);
    let bad = service.repl(token, "{".into()).await.unwrap();
    assert!(bad.contains("error"));
    let gone = service
        .repl(
            "missing".into(),
            r#"{"op":"extract","what":"links"}"#.into(),
        )
        .await
        .unwrap();
    assert!(gone.contains("expired"));
}

#[test]
fn caller_limits_cannot_expand_any_module_ceiling() {
    use tinyjuice_bus::repl::ReplLimits;
    let unlimited = ReplLimits {
        max_hits: usize::MAX,
        max_lines: usize::MAX,
        max_output_chars: usize::MAX,
        max_line_chars: usize::MAX,
        regex_size_limit: usize::MAX,
    };
    assert_eq!(query_limits(unlimited), ReplLimits::default());
    let narrowed = ReplLimits {
        max_hits: 1,
        max_lines: 2,
        max_output_chars: 3,
        max_line_chars: 4,
        regex_size_limit: 5,
    };
    assert_eq!(query_limits(narrowed), narrowed);
}

#[tokio::test]
async fn oversized_supplied_inputs_are_rejected_before_execution() {
    use tinyjuice_bus::wire::*;
    let query = QueryRequest {
        target: QueryTarget::Content {
            content: "x".repeat(MAX_QUERY_CONTENT_BYTES + 1),
        },
        op: serde_json::from_value(serde_json::json!({"op":"find","query":""})).unwrap(),
        limits: Default::default(),
        context_token: None,
        scope: None,
    };
    assert_eq!(
        Compression::default().query(query).await.unwrap(),
        Err(QueryError::InputTooLarge)
    );
    assert_eq!(
        Compression::default()
            .extract_html("x".repeat(MAX_HTML_INPUT_BYTES + 1))
            .await
            .unwrap(),
        Err(HtmlError::InputTooLarge)
    );
}

#[tokio::test]
async fn service_detects_compacts_and_reads_ranges() {
    use tinyjuice::cache::CcrStore;
    let service = Compression::default();
    assert_eq!(
        service
            .detect(
                "{}".into(),
                ContentHint {
                    explicit: Some(tinyjuice::types::ContentKind::Json),
                    ..Default::default()
                }
            )
            .await
            .unwrap(),
        "json"
    );
    let compact = service
        .compact(
            "short output".into(),
            "bash".into(),
            false,
            AgentTokenjuiceCompression::default(),
        )
        .await
        .unwrap();
    assert_eq!(compact.text, "short output");
    assert!(!compact.applied);
    assert_eq!(compact.original_bytes, compact.compacted_bytes);
    assert_eq!(compact.original_tokens, compact.compacted_tokens);
    let compact_with = service
        .compact_with(CompactRequest {
            content: "short output".into(),
            tool_name: "bash".into(),
            enabled: false,
            profile: AgentTokenjuiceCompression::default(),
            arguments: None,
            focus: None,
            context_token: None,
            scope: None,
        })
        .await
        .unwrap();
    assert_eq!(compact_with.text, compact.text);
    let token = service
        .state
        .ccr
        .put("alpha\nbeta\ngamma")
        .token()
        .to_string();
    for (unit, start, end, expected) in [
        (WireRangeUnit::Bytes, 0, 5, "alpha"),
        (WireRangeUnit::Lines, 1, 2, "beta"),
    ] {
        assert_eq!(
            service
                .retrieve(token.clone(), Some(RetrieveRange { unit, start, end }))
                .await
                .unwrap()
                .as_deref(),
            Some(expected)
        );
    }
    assert!(service.cache_stats().await.unwrap().entries > 0);
    assert_eq!(
        service
            .extract_html("<h1>Title</h1>".into())
            .await
            .unwrap()
            .unwrap()
            .trim(),
        "# Title"
    );
}

#[tokio::test]
async fn typed_queries_preserve_results_and_structured_errors() {
    use tinyjuice::cache::CcrStore;
    use tinyjuice_bus::wire::{QueryError, QueryRequest, QueryTarget};
    let service = Compression::default();
    let token = service
        .state
        .ccr
        .put("alpha\nbeta needle")
        .token()
        .to_string();
    for target in [
        QueryTarget::Handle { token },
        QueryTarget::Content {
            content: "alpha\nbeta needle".into(),
        },
    ] {
        let result = service
            .query(QueryRequest {
                target,
                op: serde_json::from_value(serde_json::json!({"op":"find","query":"needle"}))
                    .unwrap(),
                limits: Default::default(),
                context_token: Some("ticket".into()),
                scope: Some("turn".into()),
            })
            .await
            .unwrap()
            .unwrap();
        assert!(
            serde_json::to_string(&result)
                .unwrap()
                .contains("beta needle")
        );
    }
    for (target, op, expected) in [
        (
            QueryTarget::Handle {
                token: "missing".into(),
            },
            serde_json::json!({"op":"find","query":"needle"}),
            QueryError::HandleNotFound,
        ),
        (
            QueryTarget::Content {
                content: "alpha".into(),
            },
            serde_json::json!({"op":"find","query":""}),
            QueryError::EmptyQuery,
        ),
    ] {
        assert_eq!(
            service
                .query(QueryRequest {
                    target,
                    op: serde_json::from_value(op).unwrap(),
                    limits: Default::default(),
                    context_token: None,
                    scope: None,
                })
                .await
                .unwrap(),
            Err(expected)
        );
    }
    let result = service
        .query(QueryRequest {
            target: QueryTarget::Content {
                content: "alpha".into(),
            },
            op: serde_json::from_value(serde_json::json!({"op":"find","query":"[","mode":"regex"}))
                .unwrap(),
            limits: Default::default(),
            context_token: None,
            scope: None,
        })
        .await
        .unwrap();
    assert!(matches!(result, Err(QueryError::InvalidPattern(_))));
}

#[tokio::test]
async fn savings_service_records_and_resets_totals() {
    let service = Compression::default();
    service.configure_savings("ignored".into()).await.unwrap();
    service.reset_savings().await.unwrap();
    service
        .record_savings(tinyjuice_bus::savings::RecordSavingsRequest {
            model: "test-model".into(),
            compressor: "test-compressor".into(),
            original_tokens: 100,
            compacted_tokens: 40,
            input_per_mtok_usd: Some(2.0),
        })
        .await
        .unwrap();
    let stats = service.savings_stats().await.unwrap();
    assert_eq!(stats.total.tokens_saved, 60);
    assert_eq!(stats.by_model["test-model"].events, 1);
    service.reset_savings().await.unwrap();
    assert_eq!(service.savings_stats().await.unwrap().total.events, 0);
}

#[derive(Clone)]
struct SummaryHost;

#[tinybus::interface(name = "ai.tinyhumans.tinyjuice.MlHost")]
impl SummaryHost {
    async fn generate(&self, request: GenerateRequest) -> BusResult<Option<String>> {
        assert_eq!(request.purpose, "tool_output_summary");
        assert_eq!(request.context_token, "turn");
        Ok(Some("host summary".into()))
    }
}

#[tokio::test]
async fn setup_serves_detection_and_forwards_model_calls_to_the_host() {
    use tinybus::broker::Broker;
    use tinybus::transport::memory::MemoryBus;
    let bus = MemoryBus::new();
    let broker_task = Broker::new().spawn(bus.clone());
    let host = Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    host.serve_at(ML_HOST_PATH.try_into().unwrap(), SummaryHost)
        .await
        .unwrap();
    host.request_name(ML_HOST_NAME).await.unwrap();
    let module = Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    setup_connection_services(module).await.unwrap();
    let proxy = host.proxy(BUS_NAME, OBJECT_PATH, BUS_NAME).unwrap();
    let detected: String = proxy
        .call(
            "Detect",
            (
                "sample".to_string(),
                ContentHint {
                    explicit: Some(tinyjuice::types::ContentKind::Log),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
    assert_eq!(detected, "log");
    proxy
        .call::<()>(
            "Install",
            (serde_json::json!({
                "options": {
                    "routerEnabled": true,
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
        .unwrap();
    let response: CompactResponse = proxy
        .call(
            "CompactWith",
            (CompactRequest {
                content: "summarize this artifact ".repeat(40),
                tool_name: "web_fetch".into(),
                enabled: false,
                profile: AgentTokenjuiceCompression::Full,
                arguments: None,
                focus: None,
                context_token: Some("turn".into()),
                scope: Some("setup-summary".into()),
            },),
        )
        .await
        .unwrap();
    assert_eq!(response.compressor, "llm_summary");
    assert!(response.text.starts_with("host summary"));
    broker_task.abort();
}
