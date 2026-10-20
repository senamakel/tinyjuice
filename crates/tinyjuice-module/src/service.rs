use std::sync::{Arc, RwLock};

use serde::Deserialize;
use tinybus::{Connection, Result as BusResult};
use tinybus_module::ModuleSetupContext;
use tinyjuice::cache::store::{CcrStore, RangeUnit};
use tinyjuice::llm::{GenerateCallback, GenerateFuture};
use tinyjuice::summarize::SummaryState;
use tinyjuice::types::{AgentTokenjuiceCompression, CompressedOutput, ContentHint};

// The interface's vocabulary is the contract crate's, not this adapter's.
// These five used to be private structs in this file, which meant a host had
// no way to reach them and re-declared its own — the drift a shared contract
// exists to remove.
use tinyjuice::tool_integration::ToolOutputCall;
use tinyjuice_bus::names::ml_host;
pub use tinyjuice_bus::names::{BUS_NAME, ML_HOST_NAME, ML_HOST_PATH, OBJECT_PATH};
use tinyjuice_bus::wire::{
    CacheStats, CompactRequest, CompactResponse, GenerateRequest, InstallRequest,
    RangeUnit as WireRangeUnit, RetrieveRange,
};

/// Deadline for one host model call. A summary of a large page takes tens of
/// seconds, well past the bus default; this bounds a hung host, not a slow one.
const GENERATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// One published Compression service and its immutable storage/profile ports.
#[derive(Clone)]
pub(crate) struct Compression {
    state: Arc<CompressionState>,
}

struct CompressionState {
    connection: Option<Connection>,
    storage: Option<crate::storage::StorageBinding>,
    require_storage: bool,
    options: RwLock<tinyjuice::types::CompressOptions>,
    ccr: crate::storage::InstanceCcrStore,
    summaries: SummaryState,
    savings: crate::savings_store::Savings,
    generate: Arc<GenerateCallback>,
}

impl Compression {
    pub(crate) fn new(
        connection: Option<Connection>,
        storage: Option<crate::storage::StorageBinding>,
        require_storage: bool,
    ) -> Self {
        let generate = connection.as_ref().map_or_else(
            || {
                Arc::new(|_: GenerateRequest| Box::pin(async { Ok(None) }) as GenerateFuture)
                    as Arc<GenerateCallback>
            },
            generate_callback,
        );
        let savings = storage.as_ref().filter(|_| require_storage).map_or_else(
            crate::savings_store::Savings::new,
            |storage| {
                crate::savings_store::Savings::with_documents(
                    Arc::clone(&storage.documents),
                    require_storage,
                )
            },
        );
        Self {
            state: Arc::new(CompressionState {
                savings,
                connection,
                storage,
                require_storage,
                options: RwLock::new(tinyjuice::types::CompressOptions::default()),
                ccr: crate::storage::InstanceCcrStore::new(),
                summaries: SummaryState::default(),
                generate,
            }),
        }
    }

    fn options(&self) -> tinyjuice::types::CompressOptions {
        self.state
            .options
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Default for Compression {
    fn default() -> Self {
        Self::new(None, None, false)
    }
}

fn generate_callback(connection: &Connection) -> Arc<GenerateCallback> {
    let connection = connection.clone();
    Arc::new(move |request: GenerateRequest| {
        let connection = connection.clone();
        Box::pin(async move {
            let proxy = connection
                .proxy(ML_HOST_NAME, ML_HOST_PATH, ML_HOST_NAME)
                .map_err(|error| error.to_string())?
                .with_timeout(GENERATE_TIMEOUT);
            proxy
                .call(ml_host::GENERATE, (request,))
                .await
                .map_err(|error| error.to_string())
        }) as GenerateFuture
    })
}

#[tinybus::interface(name = "ai.tinyhumans.tinyjuice.Compression")]
impl Compression {
    pub(crate) async fn install(&self, request: InstallRequest) -> BusResult<()> {
        let ttl = request.ccr_ttl_secs.map(std::time::Duration::from_secs);
        let durable = if request.ccr_durable {
            if self.state.require_storage {
                match self.state.storage.as_ref() {
                    Some(storage) => {
                        match crate::storage::durable_tier_on(Arc::clone(&storage.blobs), ttl).await
                        {
                            Ok(durable) => Some(durable),
                            Err(error) => {
                                log::warn!(
                                    "[tinyjuice_module] configured CCR store unavailable: {error}"
                                );
                                return Err(tinybus::Error::failed(
                                    "configured CCR storage unavailable",
                                ));
                            }
                        }
                    }
                    None => {
                        return Err(tinybus::Error::failed("configured CCR storage unavailable"));
                    }
                }
            } else {
                match self.state.connection.as_ref() {
                    Some(connection) => {
                        crate::storage::legacy_durable_tier(connection.clone(), ttl).await
                    }
                    None => None,
                }
            }
        } else {
            None
        };
        self.state
            .ccr
            .configure(request.max_cache_entries, request.max_cache_bytes, ttl);
        self.state.ccr.set_durable(durable);
        *self
            .state
            .options
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = request.options;
        Ok(())
    }

    async fn detect(&self, content: String, hint: ContentHint) -> BusResult<String> {
        Ok(tinyjuice::detect_content_kind(&content, &hint)
            .as_str()
            .to_string())
    }

    pub(crate) async fn compress(
        &self,
        content: String,
        hint: ContentHint,
    ) -> BusResult<CompressedOutput> {
        Ok(tinyjuice::compress_content_with_store(
            &content,
            Some(hint),
            &self.options(),
            &self.state.ccr,
        )
        .await)
    }

    async fn compact(
        &self,
        content: String,
        tool_name: String,
        enabled: bool,
        profile: AgentTokenjuiceCompression,
    ) -> BusResult<CompactResponse> {
        Ok(compact_request(
            &self.state,
            CompactRequest {
                content,
                tool_name,
                enabled,
                profile,
                arguments: None,
                focus: None,
                context_token: None,
                scope: None,
            },
        )
        .await)
    }

    async fn compact_with(&self, request: CompactRequest) -> BusResult<CompactResponse> {
        Ok(compact_request(&self.state, request).await)
    }

    async fn retrieve(
        &self,
        token: String,
        range: Option<RetrieveRange>,
    ) -> BusResult<Option<String>> {
        Ok(match range {
            Some(range) => self.state.ccr.get_range(
                &token,
                range.start,
                range.end,
                match range.unit {
                    WireRangeUnit::Bytes => RangeUnit::Bytes,
                    WireRangeUnit::Lines => RangeUnit::Lines,
                },
            ),
            None => self.state.ccr.get(&token),
        })
    }

    /// Inspect a stored output with a REPL op. `op` is a JSON `ReplOp`
    /// (`{"op":"grep","pattern":"ERROR"}`); the reply is a JSON `ReplOutput`, or
    /// `{"error": "..."}` for a bad op, pattern or expired handle.
    async fn repl(&self, handle: String, op: String) -> BusResult<String> {
        let reply = match serde_json::from_str::<tinyjuice::repl::ReplOp>(&op) {
            Err(e) => serde_json::json!({ "error": format!("invalid op: {e}") }),
            Ok(op) => match tinyjuice::repl::run_op(
                &self.state.ccr,
                &handle,
                &op,
                &tinyjuice::repl::ReplLimits::default(),
            ) {
                Ok(out) => serde_json::to_value(out).unwrap_or_default(),
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            },
        };
        Ok(reply.to_string())
    }

    /// A typed query always executes against module-owned storage or supplied text.
    async fn query(
        &self,
        request: tinyjuice_bus::wire::QueryRequest,
    ) -> BusResult<tinyjuice_bus::wire::QueryResponse> {
        use tinyjuice_bus::wire::{QueryError, QueryTarget};
        let limits = query_limits(request.limits);
        let model = request
            .context_token
            .map(|context_token| tinyjuice::repl::ModelSummary {
                options: self.options(),
                context_token,
                scope: request.scope,
            });
        let result = match request.target {
            QueryTarget::Handle { token } => {
                tinyjuice::repl::run_op_with_model_with_context(
                    &self.state.ccr,
                    &token,
                    &request.op,
                    &limits,
                    model.as_ref(),
                    &self.state.summaries,
                    self.state.generate.clone(),
                )
                .await
            }
            QueryTarget::Content { content } => {
                if content.len() > tinyjuice_bus::wire::MAX_QUERY_CONTENT_BYTES {
                    return Ok(Err(QueryError::InputTooLarge));
                }
                tinyjuice::repl::run_on_text_with_model_with_context(
                    &content,
                    &request.op,
                    &limits,
                    model.as_ref(),
                    &self.state.summaries,
                    self.state.generate.clone(),
                )
                .await
            }
        };
        Ok(result.map_err(|error| match error {
            tinyjuice::repl::ReplError::HandleNotFound => QueryError::HandleNotFound,
            tinyjuice::repl::ReplError::InvalidPattern(detail) => {
                QueryError::InvalidPattern(detail)
            }
            tinyjuice::repl::ReplError::EmptyQuery => QueryError::EmptyQuery,
            tinyjuice::repl::ReplError::Unsupported(detail) => {
                QueryError::Unsupported(detail.into())
            }
        }))
    }

    async fn extract_html(&self, content: String) -> BusResult<tinyjuice_bus::wire::HtmlResponse> {
        if content.len() > tinyjuice_bus::wire::MAX_HTML_INPUT_BYTES {
            return Ok(Err(tinyjuice_bus::wire::HtmlError::InputTooLarge));
        }
        Ok(Ok(tinyjuice::compressors::html::html_to_markdown(&content)))
    }

    async fn cache_stats(&self) -> BusResult<CacheStats> {
        let (entries, bytes) = self.state.ccr.stats();
        Ok(CacheStats { entries, bytes })
    }

    /// Deprecated no-op (contract 2.3): savings live in the host's document
    /// store, so there is no snapshot path to select.
    async fn configure_savings(&self, _snapshot_path: String) -> BusResult<()> {
        Ok(())
    }

    /// Accumulate a compaction using the host-provided model input price.
    async fn record_savings(
        &self,
        request: tinyjuice_bus::savings::RecordSavingsRequest,
    ) -> BusResult<()> {
        self.state
            .savings
            .record(self.state.connection.as_ref(), &request)
            .await
    }

    /// Return a consistent copy of the module-owned savings totals.
    async fn savings_stats(&self) -> BusResult<tinyjuice_bus::savings::SavingsAggregate> {
        self.state
            .savings
            .stats(self.state.connection.as_ref())
            .await
    }

    /// Clear module-owned savings, in memory and in storage.
    async fn reset_savings(&self) -> BusResult<()> {
        self.state
            .savings
            .reset(self.state.connection.as_ref())
            .await
    }

    /// Adopt a legacy snapshot when nothing was recorded yet.
    async fn import_savings(
        &self,
        legacy: tinyjuice_bus::savings::SavingsAggregate,
    ) -> BusResult<bool> {
        self.state
            .savings
            .import(self.state.connection.as_ref(), legacy)
            .await
    }
}

/// Requests may narrow a budget but never expand the module's stock ceilings.
fn query_limits(requested: tinyjuice_bus::repl::ReplLimits) -> tinyjuice_bus::repl::ReplLimits {
    use tinyjuice_bus::repl::ReplLimits;
    let ceiling = ReplLimits::default();
    ReplLimits {
        max_hits: requested.max_hits.min(ceiling.max_hits),
        max_lines: requested.max_lines.min(ceiling.max_lines),
        max_output_chars: requested.max_output_chars.min(ceiling.max_output_chars),
        max_line_chars: requested.max_line_chars.min(ceiling.max_line_chars),
        regex_size_limit: requested.regex_size_limit.min(ceiling.regex_size_limit),
    }
}

/// `Compact` and `CompactWith` share one body; the positional form is
/// `CompactWith` with no arguments, focus or context. `enabled = false` turns
/// the content router off but not the summary stage, which is gated by its
/// own option and a context token.
async fn compact_request(state: &CompressionState, request: CompactRequest) -> CompactResponse {
    let CompactRequest {
        content,
        tool_name,
        enabled,
        profile,
        arguments,
        focus,
        context_token,
        scope,
    } = request;
    let original_tokens = tinyjuice::tokens::estimate_tokens(&content);
    let options = state
        .options
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let report = tinyjuice::tool_integration::compact_tool_output_with_context(
        ToolOutputCall {
            tool_name: &tool_name,
            arguments: arguments.as_ref(),
            output: &content,
            exit_code: None,
            profile,
            compaction_enabled: enabled,
            focus: focus.as_deref(),
            context_token: context_token.as_deref(),
            scope: scope.as_deref(),
        },
        &options,
        &state.ccr,
        &state.summaries,
        state.generate.clone(),
    )
    .await;
    let stats = report.stats;
    let compacted_tokens = tinyjuice::tokens::estimate_tokens(&report.text);
    let (compressor, content_kind) = stats.rule_id.strip_prefix("none/").map_or_else(
        || (stats.rule_id.clone(), "plain_text".to_string()),
        |kind| ("none".to_string(), kind.to_string()),
    );
    CompactResponse {
        text: report.text,
        original_bytes: stats.original_bytes,
        compacted_bytes: stats.compacted_bytes,
        rule_id: stats.rule_id,
        applied: stats.applied,
        content_kind,
        compressor,
        original_tokens,
        compacted_tokens,
        notice: report.notice.map(str::to_string),
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StorageSetupOptions {
    storage_descriptor: Option<tinystoragedrivers_busproxy::governed::StorageDescriptor>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ModuleSetupConfig {
    Null(()),
    Options(StorageSetupOptions),
}

impl ModuleSetupConfig {
    fn storage_descriptor(
        self,
    ) -> Option<tinystoragedrivers_busproxy::governed::StorageDescriptor> {
        match self {
            Self::Null(()) => None,
            Self::Options(options) => options.storage_descriptor,
        }
    }
}

async fn setup(context: ModuleSetupContext, config: ModuleSetupConfig) -> BusResult<()> {
    crate::migration::serve(&context).await?;
    let connection = context.connection().clone();
    let descriptor = config.storage_descriptor();
    let require_storage = descriptor.is_some();
    let storage = crate::storage::bind(connection.clone(), descriptor).await?;
    setup_connection_services_with_storage(connection, storage, require_storage).await
}

#[cfg(test)]
async fn setup_connection_services(connection: Connection) -> BusResult<()> {
    let storage = crate::storage::bind(connection.clone(), None).await?;
    setup_connection_services_with_storage(connection, storage, false).await
}

async fn setup_connection_services_with_storage(
    connection: Connection,
    storage: Option<crate::storage::StorageBinding>,
    require_storage: bool,
) -> BusResult<()> {
    connection
        .serve_at(
            OBJECT_PATH.try_into()?,
            Compression::new(Some(connection.clone()), storage, require_storage),
        )
        .await?;
    connection.request_name(BUS_NAME).await?;
    Ok(())
}

#[allow(missing_docs, unreachable_pub)]
pub(crate) mod exports {
    tinybus_module::module_export_optional_static! {
        setup_with_context = super::setup,
        config = super::ModuleSetupConfig,
        worker_threads = 2,
        provides = [
            "ai.tinyhumans.tinyjuice.Compression",
            "org.tinystoragedrivers.Migration",
        ],
        methods = [
            "Install",
            "Detect",
            "Compress",
            "Compact",
            "CompactWith",
            "Retrieve",
            "Repl",
            "Query",
            "ExtractHtml",
            "CacheStats",
            "ConfigureSavings",
            "RecordSavings",
            "SavingsStats",
            "ResetSavings",
            "ImportSavings"
        ],
        signals = [],
        requires = [],
        optional = ["ai.tinyhumans.tinyjuice.MlHost"],
        lazy = false,
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
