use super::*;
use serde_json::json;
use std::io::{Cursor, Write};
use std::sync::Mutex;
use tinyjuice_bus::repl::{Hit, ReplOutput};

#[test]
fn artifact_opened_after_path_replacement_is_rejected_by_file_identity() {
    let temp = tempfile::tempdir().unwrap();
    let expected = temp.path().join("authorized.txt");
    let replacement = temp.path().join("outside.txt");
    std::fs::write(&expected, "authorized").unwrap();
    std::fs::write(&replacement, "outside").unwrap();

    let canonical_dir = std::fs::canonicalize(temp.path()).unwrap();
    let expected_path = std::fs::canonicalize(&expected).unwrap();
    let expected_metadata = std::fs::metadata(&expected).unwrap();
    let opened_file = std::fs::File::open(&replacement).unwrap();
    let opened_metadata = opened_file.metadata().unwrap();
    assert!(!same_open_file_identity(
        &expected_metadata,
        &opened_metadata,
        &expected,
        &opened_file,
    ));
    let error = validate_opened_artifact(
        &canonical_dir,
        &expected_path,
        &expected_metadata,
        &opened_metadata,
        &opened_file,
        u64::MAX,
    )
    .expect_err("replacement file identity must be refused");
    assert!(error.contains("that artifact changed while its access was being checked"));
}

#[test]
fn opened_artifact_growth_is_checked_against_the_size_cap() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("artifact.txt");
    std::fs::write(&path, "small").unwrap();
    let canonical_dir = std::fs::canonicalize(temp.path()).unwrap();
    let canonical_path = std::fs::canonicalize(&path).unwrap();
    let expected_metadata = std::fs::metadata(&path).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"-grew")
        .unwrap();
    let opened_metadata = file.metadata().unwrap();

    let error = validate_opened_artifact(
        &canonical_dir,
        &canonical_path,
        &expected_metadata,
        &opened_metadata,
        &file,
        expected_metadata.len(),
    )
    .expect_err("growth after the initial stat must not exceed the read cap");
    assert!(error.contains("over the 5-byte limit"));
}

#[test]
fn opened_artifact_outside_tool_results_is_refused_even_when_metadata_matches() {
    let workspace = tempfile::tempdir().unwrap();
    let artifact_dir = workspace.path().join("artifacts/tool-results");
    std::fs::create_dir_all(&artifact_dir).unwrap();
    let outside = workspace.path().join("outside.txt");
    std::fs::write(&outside, "outside").unwrap();

    let canonical_artifact_dir = std::fs::canonicalize(&artifact_dir).unwrap();
    let outside_path = std::fs::canonicalize(&outside).unwrap();
    let expected = std::fs::metadata(&outside).unwrap();
    let file = std::fs::File::open(&outside).unwrap();
    let opened = file.metadata().unwrap();
    let error = validate_opened_artifact(
        &canonical_artifact_dir,
        &outside_path,
        &expected,
        &opened,
        &file,
        u64::MAX,
    )
    .expect_err("opened handle outside the artifact directory must be refused");
    assert!(error.contains("that path resolves outside the tool-results directory"));
}

#[test]
fn bounded_artifact_read_rejects_bytes_beyond_the_limit() {
    let error = read_bounded_artifact_bytes(Cursor::new(b"four"), 3).unwrap_err();
    assert!(error.contains("over the 3-byte limit"));
}

#[derive(Default)]
struct Capture {
    context_addresses: Mutex<Vec<Option<usize>>>,
    thread_ids: Mutex<Vec<Option<String>>>,
    requests: Mutex<Vec<QueryRequest>>,
    response: Mutex<Option<QueryResponse>>,
    transport_error: Mutex<Option<String>>,
}

#[async_trait]
impl QuerySource for Capture {
    async fn query(
        &self,
        context: Option<&dyn ToolRunContext>,
        request: QueryRequest,
    ) -> Result<QueryResponse, String> {
        self.context_addresses
            .lock()
            .unwrap()
            .push(context.map(|ctx| {
                let ptr: *const dyn ToolRunContext = ctx;
                ptr as *const () as usize
            }));
        self.thread_ids.lock().unwrap().push(
            context
                .and_then(ToolRunContext::thread_id)
                .map(str::to_owned),
        );
        self.requests.lock().unwrap().push(request);
        if let Some(error) = self.transport_error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(self.response.lock().unwrap().clone().unwrap_or_else(|| {
            Ok(ReplOutput::Text {
                text: "result".into(),
            })
        }))
    }
}

struct RunContext {
    thread_id: String,
}

impl ToolRunContext for RunContext {
    fn thread_id(&self) -> Option<&str> {
        Some(&self.thread_id)
    }
}

fn tools(source: Arc<dyn QuerySource>) -> Vec<Box<dyn Tool>> {
    repl_tools(source, ReplLimits::default(), None, 1024, None)
}

#[tokio::test]
async fn tool_execution_sends_the_module_request_and_exact_caller_context() {
    let source = Arc::new(Capture::default());
    let run = RunContext {
        thread_id: "thread-a".into(),
    };
    let expected_address = &run as *const RunContext as usize;
    let mut registered = tools(Arc::clone(&source) as Arc<dyn QuerySource>);
    let tool = registered.remove(0);
    let result = tool
        .execute_with_context(
            json!({"handle":"⟦tj:abc123⟧","query":"needle"}),
            ToolCallOptions::default(),
            Some(&run),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(
        source.context_addresses.lock().unwrap().as_slice(),
        &[Some(expected_address)]
    );
    assert_eq!(
        source.thread_ids.lock().unwrap().as_slice(),
        &[Some("thread-a".into())]
    );
    let requests = source.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].target,
        QueryTarget::Handle {
            token: "abc123".into()
        }
    );
    assert_eq!(requests[0].context_token, None);
    assert!(matches!(&requests[0].op, ReplOp::Find { query, .. } if query == "needle"));
}

#[tokio::test]
async fn separate_callers_keep_distinct_context_identity_and_thread_ids() {
    let source = Arc::new(Capture::default());
    let mut registered = tools(Arc::clone(&source) as Arc<dyn QuerySource>);
    let tool = registered.remove(0);
    let first = RunContext {
        thread_id: "thread-a".into(),
    };
    let second = RunContext {
        thread_id: "thread-b".into(),
    };
    for context in [&first, &second] {
        tool.execute_with_context(
            json!({"handle":"abc123","query":"q"}),
            ToolCallOptions::default(),
            Some(context),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        source.context_addresses.lock().unwrap().as_slice(),
        &[
            Some(&first as *const RunContext as usize),
            Some(&second as *const RunContext as usize),
        ]
    );
    assert_eq!(
        source.thread_ids.lock().unwrap().as_slice(),
        &[Some("thread-a".into()), Some("thread-b".into())]
    );
}

#[tokio::test]
async fn absence_of_context_stays_absent_and_module_errors_are_mapped() {
    let source = Arc::new(Capture::default());
    *source.response.lock().unwrap() = Some(Err(QueryError::InvalidPattern("[".into())));
    let mut registered = tools(Arc::clone(&source) as Arc<dyn QuerySource>);
    let tool = registered.remove(0);
    let result = tool
        .execute(json!({"handle":"abc123","query":"["}))
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(result.output(), "juice: invalid pattern: [");
    assert_eq!(source.context_addresses.lock().unwrap().as_slice(), &[None]);
}

#[tokio::test]
async fn missing_handles_map_to_a_failed_tool_result_without_retry_advice() {
    let source = Arc::new(Capture::default());
    *source.response.lock().unwrap() = Some(Err(QueryError::HandleNotFound));
    let mut registered = tools(Arc::clone(&source) as Arc<dyn QuerySource>);
    let tool = registered.remove(0);
    let result = tool
        .execute(json!({"handle":"abc123","query":"x"}))
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(result.output().contains("Do NOT re-run"));
}

#[tokio::test]
async fn module_and_transport_errors_have_stable_tool_messages() {
    let cases = [
        (
            QueryError::InputTooLarge,
            "juice: input exceeds module limit",
        ),
        (QueryError::EmptyQuery, "juice: empty query"),
        (
            QueryError::Unsupported("jq".into()),
            "juice: jq support is not compiled in",
        ),
    ];
    for (error, expected) in cases {
        let source = Arc::new(Capture::default());
        *source.response.lock().unwrap() = Some(Err(error));
        let mut registered = tools(Arc::clone(&source) as Arc<dyn QuerySource>);
        let result = registered
            .remove(0)
            .execute(json!({"handle":"abc123","query":"q"}))
            .await
            .unwrap();
        assert!(result.is_error);
        assert_eq!(result.output(), expected);
    }

    let source = Arc::new(Capture::default());
    *source.transport_error.lock().unwrap() = Some("module disconnected".into());
    let mut registered = tools(Arc::clone(&source) as Arc<dyn QuerySource>);
    let result = registered
        .remove(0)
        .execute(json!({"handle":"abc123","query":"q"}))
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(result.output(), "juice: module disconnected");
}

#[tokio::test]
async fn the_normal_factory_dispatches_find_extract_and_summarize() {
    let source = Arc::new(Capture::default());
    let registered = tools(Arc::clone(&source) as Arc<dyn QuerySource>);
    for (tool, arguments) in registered.iter().zip([
        json!({"handle":"abc123","query":"q"}),
        json!({"handle":"abc123","what":"links"}),
        json!({"handle":"abc123"}),
    ]) {
        tool.execute(arguments).await.unwrap();
    }
    let requests = source.requests.lock().unwrap();
    assert!(matches!(requests[0].op, ReplOp::Find { .. }));
    assert!(matches!(requests[1].op, ReplOp::Extract { .. }));
    assert!(matches!(requests[2].op, ReplOp::Summarize { .. }));
}

#[tokio::test]
async fn frozen_declarations_keep_the_recorded_surface_and_execute_the_recorded_operation() {
    let source = Arc::new(Capture::default());
    let declaration = ReplToolDeclaration {
        name: "recorded_extract".into(),
        op: "extract".into(),
        description: "recorded description".into(),
        parameters: json!({"type":"object","properties":{"handle":{"type":"string"},"query":{"type":"string"},"old_field":{"type":"integer"}}}),
    };
    let parameters = declaration.parameters.clone();
    let mut registered = repl_tools_with_declarations(
        vec![declaration],
        Arc::clone(&source) as Arc<dyn QuerySource>,
        ReplLimits {
            max_output_chars: 321,
            ..ReplLimits::default()
        },
        None,
        1024,
        Some("turn-token".into()),
    );
    let tool = registered.remove(0);
    assert_eq!(tool.name(), "recorded_extract");
    assert_eq!(tool.description(), "recorded description");
    assert_eq!(tool.parameters_schema(), parameters);
    assert_eq!(tool.max_result_size_chars(), Some(642));
    tool.execute(json!({"handle":"abc123","what":"headings"}))
        .await
        .unwrap();
    assert!(matches!(
        source.requests.lock().unwrap()[0].op,
        ReplOp::Extract { .. }
    ));
}

#[tokio::test]
async fn authorized_artifact_content_is_read_with_the_injected_cap_then_sent_to_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("artifacts/tool-results");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("result.txt");
    std::fs::write(&file, "artifact body").unwrap();
    let source = Arc::new(Capture::default());
    let run = RunContext {
        thread_id: "artifact-thread".into(),
    };
    let expected_address = &run as *const RunContext as usize;
    let tool = repl_tools(
        Arc::clone(&source) as Arc<dyn QuerySource>,
        ReplLimits::default(),
        Some(dir.clone()),
        64,
        Some("summary-ticket".into()),
    )
    .remove(0);
    let result = tool
        .execute_with_context(
            json!({"handle":file.to_string_lossy(),"query":"body"}),
            ToolCallOptions::default(),
            Some(&run),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    let requests = source.requests.lock().unwrap();
    assert_eq!(
        requests[0].target,
        QueryTarget::Content {
            content: "artifact body".into()
        }
    );
    assert_eq!(requests[0].context_token.as_deref(), Some("summary-ticket"));
    assert_eq!(
        source.context_addresses.lock().unwrap().as_slice(),
        &[Some(expected_address)]
    );
    assert_eq!(
        source.thread_ids.lock().unwrap().as_slice(),
        &[Some("artifact-thread".into())]
    );
}

#[tokio::test]
async fn invalid_handles_and_refused_or_unconfigured_artifacts_never_reach_the_source() {
    let source = Arc::new(Capture::default());
    let mut registered = tools(Arc::clone(&source) as Arc<dyn QuerySource>);
    let result = registered
        .remove(0)
        .execute(json!({"handle":"call_123","query":"q"}))
        .await
        .unwrap();
    assert!(result.is_error);

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("artifacts/tool-results");
    std::fs::create_dir_all(&dir).unwrap();
    let outside = temp.path().join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    let mut registered = repl_tools(
        Arc::clone(&source) as Arc<dyn QuerySource>,
        ReplLimits::default(),
        Some(dir.clone()),
        64,
        None,
    );
    let result = registered
        .remove(0)
        .execute(json!({"handle":outside.to_string_lossy(),"query":"q"}))
        .await
        .unwrap();
    assert!(result.is_error);

    let mut registered = repl_tools(
        Arc::clone(&source) as Arc<dyn QuerySource>,
        ReplLimits::default(),
        None,
        64,
        None,
    );
    let result = registered
        .remove(0)
        .execute(json!({"handle":"artifacts/tool-results/file.txt","query":"q"}))
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(source.requests.lock().unwrap().is_empty());
}

#[test]
fn artifact_reader_accepts_supported_paths_and_enforces_containment_and_size() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("artifacts/tool-results");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("unicode.txt");
    std::fs::write(&file, "héllo\n世界").unwrap();
    assert_eq!(
        read_tool_result_artifact(&dir, &file.to_string_lossy(), 64).unwrap(),
        "héllo\n世界"
    );
    assert_eq!(
        read_tool_result_artifact(&dir, "./artifacts/tool-results/unicode.txt", 64).unwrap(),
        "héllo\n世界"
    );
    assert!(read_tool_result_artifact(&dir, "artifacts/tool-results/../escape", 64).is_err());
    assert!(
        read_tool_result_artifact(&dir, &file.to_string_lossy(), 2)
            .unwrap_err()
            .contains("over the 2-byte limit")
    );
    assert!(
        read_tool_result_artifact(&dir, &dir.to_string_lossy(), 64)
            .unwrap_err()
            .contains("not a file")
    );
    assert!(
        read_tool_result_artifact(&dir, "artifacts/tool-results/missing", 64)
            .unwrap_err()
            .contains("no longer exists")
    );
}

#[cfg(unix)]
#[test]
fn artifact_reader_allows_internal_symlinks_and_rejects_external_symlinks() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("artifacts/tool-results");
    std::fs::create_dir_all(&dir).unwrap();
    let internal = dir.join("inside.txt");
    let external = temp.path().join("outside.txt");
    std::fs::write(&internal, "inside").unwrap();
    std::fs::write(&external, "outside").unwrap();
    symlink(&internal, dir.join("inside-link.txt")).unwrap();
    symlink(&external, dir.join("outside-link.txt")).unwrap();
    assert_eq!(
        read_tool_result_artifact(&dir, "artifacts/tool-results/inside-link.txt", 64).unwrap(),
        "inside"
    );
    assert!(
        read_tool_result_artifact(&dir, "artifacts/tool-results/outside-link.txt", 64)
            .unwrap_err()
            .contains("resolves outside")
    );
}

#[tokio::test]
async fn execution_has_the_readonly_nonexternal_concurrency_contract() {
    let mut registered = tools(Arc::new(Capture::default()));
    let tool = registered.remove(0);
    assert_eq!(tool.permission_level(), PermissionLevel::ReadOnly);
    assert!(tool.is_concurrency_safe(&json!({"handle":"x"})));
    assert!(!tool.external_effect());
    let missing = tool.execute(json!({"query":"x"})).await.unwrap();
    assert!(missing.is_error);
}

#[test]
fn returned_output_serialization_keeps_the_bus_shape() {
    let output = ReplOutput::Lines {
        hits: vec![Hit {
            line: 1,
            text: "one".into(),
        }],
        truncated: 0,
    };
    assert_eq!(serde_json::to_value(output).unwrap()["kind"], "lines");
}
