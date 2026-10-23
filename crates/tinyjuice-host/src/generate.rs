//! Turn-bound callbacks for the module's `MlHost.Generate` member.
//!
//! A host registers a callback already bound to its active model turn and
//! sends the returned ticket token in the bus request. Each callback is
//! removed before it is awaited, and dropping its ticket withdraws it.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{LazyLock, Mutex};

pub use tinyjuice_bus::wire::GenerateRequest;

/// A single model generation callback bound to its host's turn.
pub type PreparedGenerate = Box<
    dyn FnOnce(GenerateRequest) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>
        + Send,
>;

static PENDING: LazyLock<Mutex<HashMap<String, PreparedGenerate>>> =
    LazyLock::new(Default::default);

/// A ticket that remains live until used or dropped.
#[must_use = "dropping the ticket withdraws the registered call"]
pub struct GenerateTicket {
    token: String,
}

impl GenerateTicket {
    /// Token to place in `CompactRequest::context_token`.
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for GenerateTicket {
    fn drop(&mut self) {
        if let Ok(mut pending) = PENDING.lock() {
            pending.remove(&self.token);
        }
    }
}

/// Register a callback for one module generation request.
pub fn register(prepared: PreparedGenerate) -> GenerateTicket {
    let token = uuid::Uuid::new_v4().to_string();
    if let Ok(mut pending) = PENDING.lock() {
        pending.insert(token.clone(), prepared);
    }
    log::debug!("[tinyjuice-host::generate] registered context_token={token}");
    GenerateTicket { token }
}

/// Serve one request; unknown and already-consumed tokens decline with `None`.
pub async fn serve(request: GenerateRequest) -> Result<Option<String>, String> {
    let token = request.context_token.clone();
    let prepared = PENDING
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(&token));
    let Some(prepared) = prepared else {
        log::debug!("[tinyjuice-host::generate] no registered call; declining");
        return Ok(None);
    };
    let prompt_bytes = request.prompt.len();
    let purpose = request.purpose.clone();
    let started = std::time::Instant::now();
    match prepared(request).await {
        Ok(text) => {
            log::info!(
                "[tinyjuice-host::generate] served purpose={purpose} prompt_bytes={prompt_bytes} reply_bytes={} elapsed_ms={}",
                text.len(),
                started.elapsed().as_millis()
            );
            Ok(Some(text))
        }
        Err(error) => {
            log::warn!("[tinyjuice-host::generate] model callback failed: {error}");
            Err(error.to_string())
        }
    }
}

#[cfg(test)]
#[path = "generate_tests.rs"]
mod tests;
