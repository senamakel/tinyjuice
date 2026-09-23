//! Optional host LLM callback slot.
//!
//! The summary stage ([`crate::summarize`]) writes the prompt, but the model
//! call is the host's: it owns model routing, credentials and the turn the
//! call belongs to. A host installs an async callback here; without one the
//! summary stage declines and the deterministic compressors run as before.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

pub use tinyjuice_bus::wire::GenerateRequest;

pub type GenerateFuture = Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send>>;
pub type GenerateCallback = dyn Fn(GenerateRequest) -> GenerateFuture + Send + Sync + 'static;

fn callback_cell() -> &'static RwLock<Option<Arc<GenerateCallback>>> {
    static CALLBACK: OnceLock<RwLock<Option<Arc<GenerateCallback>>>> = OnceLock::new();
    CALLBACK.get_or_init(|| RwLock::new(None))
}

/// Install or replace the host-provided generate callback.
pub fn configure_callback(callback: Option<Arc<GenerateCallback>>) {
    *callback_cell().write().unwrap_or_else(|p| p.into_inner()) = callback;
}

/// Whether a host has installed a callback.
pub fn has_callback() -> bool {
    callback_cell()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .is_some()
}

#[cfg(test)]
pub(crate) async fn callback_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    TEST_LOCK.lock().await
}

/// Run one model call through the host callback. `Ok(None)` means no callback
/// is installed or the host declined.
pub async fn generate(request: GenerateRequest) -> Result<Option<String>, String> {
    let callback = callback_cell()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let Some(callback) = callback else {
        return Ok(None);
    };
    callback(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> GenerateRequest {
        GenerateRequest {
            context_token: "turn-1".into(),
            purpose: "tool_output_summary".into(),
            system: "system".into(),
            prompt: "prompt".into(),
            max_output_tokens: 16,
        }
    }

    #[tokio::test]
    async fn declines_when_no_callback_is_configured() {
        let _guard = callback_test_guard().await;
        configure_callback(None);
        assert!(!has_callback());
        assert_eq!(generate(request()).await, Ok(None));
    }

    #[tokio::test]
    async fn delegates_to_the_configured_callback() {
        let _guard = callback_test_guard().await;
        configure_callback(Some(Arc::new(|request: GenerateRequest| {
            Box::pin(async move {
                assert_eq!(request.context_token, "turn-1");
                Ok(Some(format!("reply to {}", request.prompt)))
            })
        })));
        assert_eq!(
            generate(request()).await.unwrap().as_deref(),
            Some("reply to prompt")
        );
        configure_callback(None);
    }
}
