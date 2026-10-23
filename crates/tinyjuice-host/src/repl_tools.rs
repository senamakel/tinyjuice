//! `tinytools::Tool` adapters that send REPL requests through the bus contract.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tinyjuice_bus::handle::{self, HandleArg};
use tinyjuice_bus::repl::{ReplLimits, ReplOp};
use tinyjuice_bus::tools::{ReplToolDeclaration, repl_tool_declarations};
use tinyjuice_bus::wire::{QueryError, QueryRequest, QueryResponse, QueryTarget};
use tinytools::{PermissionLevel, Tool, ToolCallOptions, ToolResult, ToolRunContext};

/// The caller-owned source of module query results.
///
/// The run context is passed through exactly as supplied to the tool. A source
/// can use it to bind policy or runtime behavior to the invoking turn.
#[async_trait]
pub trait QuerySource: Send + Sync {
    /// Execute one bus query, preserving the original caller context.
    async fn query(
        &self,
        context: Option<&dyn ToolRunContext>,
        request: QueryRequest,
    ) -> Result<QueryResponse, String>;
}

/// Create tools from the current bus declarations.
pub fn repl_tools(
    source: Arc<dyn QuerySource>,
    limits: ReplLimits,
    artifacts_dir: Option<PathBuf>,
    max_artifact_bytes: u64,
    context_token: Option<String>,
) -> Vec<Box<dyn Tool>> {
    repl_tools_with_declarations(
        repl_tool_declarations(),
        source,
        limits,
        artifacts_dir,
        max_artifact_bytes,
        context_token,
    )
}

/// Create tools from frozen declarations recorded by the caller.
///
/// The supplied names, operations, descriptions and schemas are retained
/// verbatim, so a resumed session can execute its original tool surface while
/// using the caller's current source, limits and artifact policy.
pub fn repl_tools_with_declarations(
    declarations: Vec<ReplToolDeclaration>,
    source: Arc<dyn QuerySource>,
    limits: ReplLimits,
    artifacts_dir: Option<PathBuf>,
    max_artifact_bytes: u64,
    context_token: Option<String>,
) -> Vec<Box<dyn Tool>> {
    declarations
        .into_iter()
        .map(|declaration| {
            Box::new(ModuleReplTool {
                declaration,
                source: Arc::clone(&source),
                limits,
                artifacts_dir: artifacts_dir.clone(),
                max_artifact_bytes,
                context_token: context_token.clone(),
            }) as Box<dyn Tool>
        })
        .collect()
}

struct ModuleReplTool {
    declaration: ReplToolDeclaration,
    source: Arc<dyn QuerySource>,
    limits: ReplLimits,
    artifacts_dir: Option<PathBuf>,
    max_artifact_bytes: u64,
    context_token: Option<String>,
}

fn not_a_handle_message() -> &'static str {
    "juice: that is not a juice handle. A handle is the 32-character hex token named in a \
     stored-output footer (or its `⟦tj:<handle>⟧` marker); a tool call id such as `call_…` \
     is not one. For a `[tool_result_preview]` envelope, pass its `artifact_path` as the \
     handle, or read that file with file_read."
}

fn artifact_refused(reason: &str) -> String {
    format!(
        "juice: {reason}. Only an `artifact_path` under the workspace's \
         artifacts/tool-results directory can stand in for a handle; read other files with \
         file_read."
    )
}

/// Read a persisted tool-result artifact while enforcing its directory and size cap.
pub fn read_tool_result_artifact(dir: &Path, raw: &str, max_bytes: u64) -> Result<String, String> {
    const RELATIVE_PREFIX: &str = "artifacts/tool-results/";

    let raw = raw.trim();
    let path = Path::new(raw);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(artifact_refused("that path contains `..`"));
    }
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let forward = raw.replace('\\', "/");
        match forward
            .trim_start_matches("./")
            .strip_prefix(RELATIVE_PREFIX)
        {
            Some(rest) if !rest.is_empty() => dir.join(rest),
            _ => return Err(artifact_refused("that path is not a tool-result artifact")),
        }
    };
    let canonical_dir = std::fs::canonicalize(dir).ok();
    let lexically_inside = resolved.starts_with(dir)
        || canonical_dir
            .as_deref()
            .is_some_and(|canonical_dir| resolved.starts_with(canonical_dir));
    if !lexically_inside {
        return Err(artifact_refused("that path is not a tool-result artifact"));
    }
    let not_found = |_| artifact_refused("that artifact no longer exists");
    let canonical_dir = match canonical_dir {
        Some(canonical_dir) => canonical_dir,
        None => std::fs::canonicalize(dir).map_err(not_found)?,
    };
    let canonical = std::fs::canonicalize(&resolved).map_err(not_found)?;
    if !canonical.starts_with(&canonical_dir) {
        return Err(artifact_refused(
            "that path resolves outside the tool-results directory",
        ));
    }
    let metadata = std::fs::metadata(&canonical).map_err(not_found)?;
    if !metadata.is_file() {
        return Err(artifact_refused("that path is not a file"));
    }
    if metadata.len() > max_bytes {
        return Err(artifact_refused(&format!(
            "that artifact is {} bytes, over the {max_bytes}-byte limit",
            metadata.len()
        )));
    }
    let file = std::fs::File::open(&canonical).map_err(not_found)?;
    let opened_metadata = file.metadata().map_err(not_found)?;
    validate_opened_artifact(
        &canonical_dir,
        &canonical,
        &metadata,
        &opened_metadata,
        &file,
        max_bytes,
    )?;
    let bytes = read_bounded_artifact_bytes(file, max_bytes).map_err(|_| {
        artifact_refused(&format!("that artifact exceeds the {max_bytes}-byte limit"))
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn validate_opened_artifact(
    canonical_dir: &Path,
    expected_path: &Path,
    expected: &std::fs::Metadata,
    opened: &std::fs::Metadata,
    file: &std::fs::File,
    max_bytes: u64,
) -> Result<(), String> {
    let opened_path = opened_file_path(file)
        .ok_or_else(|| artifact_refused("the opened artifact's location could not be verified"))?;
    if !opened_path.starts_with(canonical_dir) {
        return Err(artifact_refused(
            "that path resolves outside the tool-results directory",
        ));
    }
    if opened_path != expected_path {
        return Err(artifact_refused(
            "that artifact changed while its access was being checked",
        ));
    }
    if !same_open_file_identity(expected, opened, expected_path, file) {
        return Err(artifact_refused(
            "that artifact changed while its access was being checked",
        ));
    }
    if opened.len() > max_bytes {
        return Err(artifact_refused(&format!(
            "that artifact is {} bytes, over the {max_bytes}-byte limit",
            opened.len()
        )));
    }
    Ok(())
}

fn opened_file_path(file: &std::fs::File) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        macos_opened_file_path(file).and_then(|path| std::fs::canonicalize(path).ok())
    }

    #[cfg(target_os = "linux")]
    let handle_path = {
        use std::os::fd::AsRawFd;
        std::path::PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
    };

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    let handle_path = {
        use std::os::fd::AsRawFd;
        std::path::PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()))
    };

    #[cfg(windows)]
    let handle_path = windows_opened_file_path(file)?;

    #[cfg(any(target_os = "linux", all(unix, not(target_os = "macos")), windows))]
    {
        std::fs::canonicalize(handle_path).ok()
    }

    #[cfg(not(any(unix, windows)))]
    None
}

#[cfg(target_os = "macos")]
fn macos_opened_file_path(file: &std::fs::File) -> Option<std::path::PathBuf> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStringExt};

    const F_GETPATH: i32 = 50;
    const PATH_MAX: usize = 1024;

    unsafe extern "C" {
        fn fcntl(fd: i32, command: i32, ...) -> i32;
    }

    let mut buffer = [0i8; PATH_MAX];
    let result = unsafe { fcntl(file.as_raw_fd(), F_GETPATH, buffer.as_mut_ptr()) };
    if result == -1 {
        return None;
    }
    let path = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    Some(std::path::PathBuf::from(std::ffi::OsString::from_vec(
        path.to_bytes().to_vec(),
    )))
}

#[cfg(windows)]
fn windows_opened_file_path(file: &std::fs::File) -> Option<std::path::PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFinalPathNameByHandleW(
            file: *mut std::ffi::c_void,
            file_path: *mut u16,
            file_path_size: u32,
            flags: u32,
        ) -> u32;
    }

    let handle = file.as_raw_handle().cast();
    // A zero-sized call reports the required buffer length. The API returns a
    // normalized, absolute path for the already-open handle, including the
    // Windows device prefix that canonicalize also uses.
    let required = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, 0) };
    if required == 0 {
        return None;
    }
    let mut buffer = vec![0u16; required as usize + 1];
    let written =
        unsafe { GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0) };
    if written == 0 || written as usize >= buffer.len() {
        return None;
    }
    Some(std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..written as usize],
    )))
}

fn same_open_file_identity(
    expected: &std::fs::Metadata,
    opened: &std::fs::Metadata,
    expected_path: &Path,
    opened_file: &std::fs::File,
) -> bool {
    #[cfg(not(windows))]
    let _ = (expected_path, opened_file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        expected.dev() == opened.dev() && expected.ino() == opened.ino()
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;

        #[repr(C)]
        #[derive(Default)]
        #[allow(dead_code)]
        struct FileTime {
            low: u32,
            high: u32,
        }

        #[repr(C)]
        #[derive(Default)]
        #[allow(dead_code)]
        struct ByHandleFileInformation {
            attributes: u32,
            creation_time: FileTime,
            last_access_time: FileTime,
            last_write_time: FileTime,
            volume_serial_number: u32,
            file_size_high: u32,
            file_size_low: u32,
            number_of_links: u32,
            file_index_high: u32,
            file_index_low: u32,
        }

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                file: *mut std::ffi::c_void,
                information: *mut ByHandleFileInformation,
            ) -> i32;
        }

        fn identity(file: &std::fs::File) -> Option<(u32, u32, u32)> {
            let mut information = ByHandleFileInformation::default();
            let succeeded = unsafe {
                GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information)
            };
            (succeeded != 0).then_some((
                information.volume_serial_number,
                information.file_index_high,
                information.file_index_low,
            ))
        }

        let expected_file = match std::fs::File::open(expected_path) {
            Ok(file) => file,
            Err(_) => return false,
        };
        let _ = (expected, opened);
        identity(&expected_file).is_some_and(|expected| identity(opened_file) == Some(expected))
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (expected, opened, expected_path, opened_file);
        false
    }
}

fn read_bounded_artifact_bytes(reader: impl Read, max_bytes: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read artifact: {error}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!("artifact is over the {max_bytes}-byte limit"));
    }
    Ok(bytes)
}

fn miss_message() -> &'static str {
    "juice: that handle is no longer stored (evicted, or from an earlier session). \
     Do NOT re-run the same tool call to regenerate it: the result would be stored \
     again under a new handle. Work from the preview already shown, or re-run with \
     narrower arguments so the result is small enough to keep in full."
}

impl ModuleReplTool {
    async fn query(
        &self,
        context: Option<&dyn ToolRunContext>,
        target: QueryTarget,
        mut args: Value,
    ) -> anyhow::Result<ToolResult> {
        if let Some(object) = args.as_object_mut() {
            object.insert("op".into(), Value::String(self.declaration.op.clone()));
        }
        let op: ReplOp = match serde_json::from_value(args) {
            Ok(op) => op,
            Err(error) => {
                return Ok(ToolResult::error(format!(
                    "juice: invalid arguments: {error}"
                )));
            }
        };
        let response = self
            .source
            .query(
                context,
                QueryRequest {
                    target,
                    op,
                    limits: self.limits,
                    context_token: self.context_token.clone(),
                    scope: None,
                },
            )
            .await;
        Ok(match response {
            Ok(Ok(output)) => ToolResult::json(serde_json::to_value(output)?),
            Ok(Err(QueryError::HandleNotFound)) => ToolResult::failed(miss_message()),
            Ok(Err(QueryError::InputTooLarge)) => {
                ToolResult::error("juice: input exceeds module limit")
            }
            Ok(Err(QueryError::InvalidPattern(pattern))) => {
                ToolResult::error(format!("juice: invalid pattern: {pattern}"))
            }
            Ok(Err(QueryError::EmptyQuery)) => ToolResult::error("juice: empty query"),
            Ok(Err(QueryError::Unsupported(operation))) => {
                ToolResult::error(format!("juice: {operation} support is not compiled in"))
            }
            Err(error) => ToolResult::error(format!("juice: {error}")),
        })
    }

    async fn query_artifact(
        &self,
        context: Option<&dyn ToolRunContext>,
        path: String,
        args: Value,
    ) -> anyhow::Result<ToolResult> {
        let Some(dir) = self.artifacts_dir.clone() else {
            return Ok(ToolResult::error(not_a_handle_message()));
        };
        let cap = self.max_artifact_bytes;
        let read = tokio::task::spawn_blocking(move || read_tool_result_artifact(&dir, &path, cap))
            .await
            .map_err(|error| anyhow::anyhow!("juice: artifact read task failed: {error}"))?;
        match read {
            Ok(content) => {
                self.query(context, QueryTarget::Content { content }, args)
                    .await
            }
            Err(message) => Ok(ToolResult::error(message)),
        }
    }
}

#[async_trait]
impl Tool for ModuleReplTool {
    fn name(&self) -> &str {
        &self.declaration.name
    }

    fn description(&self) -> &str {
        &self.declaration.description
    }

    fn parameters_schema(&self) -> Value {
        self.declaration.parameters.clone()
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        self.execute_with_context(args, ToolCallOptions::default(), None)
            .await
    }

    async fn execute_with_context(
        &self,
        args: Value,
        _options: ToolCallOptions,
        context: Option<&dyn ToolRunContext>,
    ) -> anyhow::Result<ToolResult> {
        let Some(raw) = args.get("handle").and_then(Value::as_str) else {
            return Ok(ToolResult::error("missing required argument: handle"));
        };
        match handle::classify(raw) {
            HandleArg::Handle(token) => {
                self.query(context, QueryTarget::Handle { token }, args)
                    .await
            }
            HandleArg::Path(path) => self.query_artifact(context, path, args).await,
            HandleArg::Invalid => Ok(ToolResult::error(not_a_handle_message())),
        }
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn is_concurrency_safe(&self, _args: &Value) -> bool {
        true
    }

    fn external_effect(&self) -> bool {
        false
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(self.limits.max_output_chars.saturating_mul(2))
    }
}

#[cfg(test)]
#[path = "repl_tools_tests.rs"]
mod tests;
