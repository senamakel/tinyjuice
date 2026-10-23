use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use tinyjuice_bus::repl::ReplLimits;
use tinyjuice_bus::tools::ReplToolDeclaration;
use tinyjuice_bus::wire::{QueryError, QueryRequest, QueryResponse, RetrieveRange};
use tinyjuice_host::generate::{GenerateRequest, PreparedGenerate, register, serve};
use tinyjuice_host::repl_tools::{QuerySource, repl_tools_with_declarations};
use tinyjuice_host::retrieve_tool::{RetrieveFn, RetrieveToolOutputTool};
use tinytools::{Tool, ToolRunContext};

struct Source;

#[async_trait]
impl QuerySource for Source {
    async fn query(
        &self,
        context: Option<&dyn ToolRunContext>,
        request: QueryRequest,
    ) -> Result<QueryResponse, String> {
        let _caller_context = context;
        let _bus_request = request;
        Ok(Err(QueryError::HandleNotFound))
    }
}

#[tokio::test]
async fn public_adapters_accept_bus_requests_and_explicit_host_inputs() {
    let source = Arc::new(Source);
    let declarations: Vec<ReplToolDeclaration> = tinyjuice_bus::tools::repl_tool_declarations();
    let tools = repl_tools_with_declarations(
        declarations,
        source,
        ReplLimits::default(),
        Some(PathBuf::from("artifacts/tool-results")),
        128,
        None,
    );
    assert_eq!(tools.len(), 3);

    let retrieve: RetrieveFn = Arc::new(|_token, _range: Option<RetrieveRange>| {
        Box::pin(async { Ok(Some("original".to_owned())) })
            as Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send>>
    });
    let tool = RetrieveToolOutputTool::new(retrieve);
    assert_eq!(tool.name(), "juice_retrieve");
    assert!(
        tool.parameters_schema()["properties"]
            .get("range")
            .is_some()
    );

    let callback: PreparedGenerate =
        Box::new(|_request: GenerateRequest| Box::pin(async { Ok("summary".to_owned()) }));
    let ticket = register(callback);
    assert!(!ticket.token().is_empty());
    let reply = serve(GenerateRequest {
        context_token: ticket.token().to_owned(),
        system: "system".to_owned(),
        prompt: "prompt".to_owned(),
        purpose: "summary".to_owned(),
        max_output_tokens: 16,
    })
    .await
    .expect("serve");
    assert_eq!(reply.as_deref(), Some("summary"));
}
