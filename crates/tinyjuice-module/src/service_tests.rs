use super::*;
use tinyjuice::types::CompressOptions;

#[tokio::test]
async fn service_compresses_and_retrieves_a_large_log() {
    // `install` reconfigures process-global cache state.
    let _install = crate::storage::tests::INSTALL.lock().await;
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

#[tokio::test]
async fn service_repl_greps_a_stored_output_and_reports_bad_input() {
    let service = Compression::default();
    use tinyjuice::cache::CcrStore;
    let token = tinyjuice::cache::GlobalCcrStore
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
    let token = tinyjuice::cache::GlobalCcrStore
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
    let token = tinyjuice::cache::GlobalCcrStore
        .put("alpha\nbeta needle")
        .token()
        .to_string();
    for target in [
        QueryTarget::Handle { token },
        QueryTarget::Content {
            content: "alpha\nbeta needle".into(),
        },
    ] {
        let result = Compression::default()
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
            Compression::default()
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
    let result = Compression::default()
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
    Compression::default()
        .configure_savings("ignored".into())
        .await
        .unwrap();
    Compression::default().reset_savings().await.unwrap();
    Compression::default()
        .record_savings(tinyjuice_bus::savings::RecordSavingsRequest {
            model: "test-model".into(),
            compressor: "test-compressor".into(),
            original_tokens: 100,
            compacted_tokens: 40,
            input_per_mtok_usd: Some(2.0),
        })
        .await
        .unwrap();
    let stats = Compression::default().savings_stats().await.unwrap();
    assert_eq!(stats.total.tokens_saved, 60);
    assert_eq!(stats.by_model["test-model"].events, 1);
    Compression::default().reset_savings().await.unwrap();
    assert_eq!(
        Compression::default()
            .savings_stats()
            .await
            .unwrap()
            .total
            .events,
        0
    );
}

#[derive(Clone)]
struct SummaryHost;

#[tinybus::interface(name = "ai.tinyhumans.tinyjuice.MlHost")]
impl SummaryHost {
    async fn generate(&self, request: GenerateRequest) -> BusResult<Option<String>> {
        assert_eq!(request.prompt, "summarize this artifact");
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
    setup(module).await.unwrap();
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
    let request: GenerateRequest = serde_json::from_value(serde_json::json!({
        "prompt": "summarize this artifact", "contextToken": "turn", "maxOutputTokens": 100,
        "purpose": "tool_output_summary", "system": "Summarize"
    }))
    .unwrap();
    assert_eq!(
        tinyjuice::llm::generate(request).await.unwrap(),
        Some("host summary".into())
    );
    tinyjuice::llm::configure_callback(None);
    broker_task.abort();
}

#[tokio::test]
async fn import_savings_at_uses_only_its_request_scoped_storage_address() {
    use tinybus::broker::Broker;
    use tinybus::transport::memory::MemoryBus;
    use tinyjuice_bus::savings::{
        ImportSavingsAtRequest, SAVINGS_DOC_ID, SavingsAggregate, SavingsBucket,
        StorageCallbackAddress,
    };
    use tinyjuice_bus::storage::SAVINGS_COLLECTION;
    use tinystoragedrivers_api::StorageBackend;
    use tinystoragedrivers_busproxy::{
        INVOCATION_OBJECT_ROOT, PerCallerEndpoint, StorageAddress, StorageServer, serve,
        serve_at_address,
    };
    use tinystoragedrivers_testkit::MemoryBackend;
    use tinystoragedrivers_types::Scope;

    let bus = MemoryBus::new();
    let broker_task = Broker::new().spawn(bus.clone());
    let host = Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    let backend = Arc::new(MemoryBackend::new());
    let ordinary_backend: Arc<dyn StorageBackend> = backend.clone();
    let ordinary_endpoint = PerCallerEndpoint::new({
        let backend = Arc::clone(&ordinary_backend);
        move |_| {
            let storage = backend.for_scope(&Scope::new("ordinary").ok()?).ok()?;
            Some(StorageServer::new(storage))
        }
    });
    let _ordinary_server = serve(&host, Arc::new(ordinary_endpoint)).await.unwrap();

    let callback_backend: Arc<dyn StorageBackend> = backend.clone();
    let callback_scope = Scope::new("invocation").unwrap();
    let callback = callback_backend.for_scope(&callback_scope).unwrap();
    let callback_docs = Arc::clone(callback.documents());
    let address = StorageAddress::try_new(
        host.unique_name().unwrap(),
        tinybus::ObjectPath::new(format!("{INVOCATION_OBJECT_ROOT}/juice_test")).unwrap(),
    )
    .unwrap();
    let _callback_server = serve_at_address(
        &host,
        address.clone(),
        Arc::new(PerCallerEndpoint::new(move |_| {
            Some(StorageServer::new(callback.clone()))
        })),
    )
    .await
    .unwrap();

    let module = Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    let savings = crate::savings_store::Savings::new();
    let initial: SavingsAggregate = savings.stats(Some(&module)).await;
    assert_eq!(initial.total.events, 0);
    let aggregate = SavingsAggregate {
        total: SavingsBucket {
            events: 2,
            original_tokens: 20,
            compacted_tokens: 8,
            tokens_saved: 12,
            cost_saved_usd: 0.25,
        },
        ..Default::default()
    };
    let imported = import_savings_at(
        &savings,
        Some(&module),
        ImportSavingsAtRequest {
            aggregate,
            callback: StorageCallbackAddress {
                host: address.host().to_string(),
                object_root: address.object_root().to_string(),
            },
        },
    )
    .await
    .unwrap();
    assert!(imported);

    assert_eq!(
        savings.stats(Some(&module)).await.total.events,
        0,
        "request-scoped import state must not become ordinary module state"
    );

    let ordinary = backend.for_scope(&Scope::new("ordinary").unwrap()).unwrap();
    let stored = ordinary
        .documents()
        .get(SAVINGS_COLLECTION, SAVINGS_DOC_ID)
        .await
        .unwrap();
    assert!(
        stored.is_none(),
        "the request port does not replace the normal port"
    );
    assert!(
        callback_docs
            .get(SAVINGS_COLLECTION, SAVINGS_DOC_ID)
            .await
            .unwrap()
            .is_some()
    );
    broker_task.abort();
}

#[tokio::test]
async fn import_savings_at_rejects_non_unique_hosts_and_non_invocation_roots() {
    use tinybus::broker::Broker;
    use tinybus::transport::memory::MemoryBus;
    use tinyjuice_bus::savings::{
        ImportSavingsAtRequest, SavingsAggregate, StorageCallbackAddress,
    };

    let bus = MemoryBus::new();
    let broker_task = Broker::new().spawn(bus.clone());
    let module = Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    let savings = crate::savings_store::Savings::new();

    let invalid_addresses = [
        StorageCallbackAddress {
            host: "ai.example.Storage".into(),
            object_root: "/ai/tinyhumans/tinystorage/Invocation/import-1".into(),
        },
        StorageCallbackAddress {
            host: ":1.7".into(),
            object_root: "/ai/tinyhumans/tinystorage/Other/import-1".into(),
        },
    ];
    for callback in invalid_addresses {
        let result = import_savings_at(
            &savings,
            Some(&module),
            ImportSavingsAtRequest {
                aggregate: SavingsAggregate::default(),
                callback,
            },
        )
        .await;
        assert!(result.is_err(), "invalid destination must be rejected");
    }
    broker_task.abort();
}
