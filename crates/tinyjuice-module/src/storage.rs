//! The module's handle on the host's storage, reached over the storage bus.
//!
//! The module owns no storage backend, file or path. The CCR store's durable
//! tier is [`tinyjuice_drivers::BlobCcrStore`] on the blob port the host
//! serves; the host binds the scope from the authenticated caller, so nothing
//! here names one. A host that does not serve storage (an older host, or a bare
//! test broker) leaves the module on its in-memory cache only.

use std::sync::Arc;
use std::time::Duration;

use tinybus::Connection;
use tinyjuice::cache::store::{CcrPutResult, CcrStore, RangeUnit};
use tinyjuice_drivers::BlobCcrStore;
use tinystoragedrivers_core::{BlobStore, DocumentStore, StorageBackend};

/// The host's blob port, connected through `connection`, when it serves one.
async fn blobs(connection: Connection) -> Option<Arc<dyn BlobStore>> {
    scoped(connection)
        .await
        .map(|scoped| Arc::clone(scoped.blobs()))
}

/// The host's document port, connected through `connection`, when it serves one.
pub(crate) async fn documents(connection: Connection) -> Option<Arc<dyn DocumentStore>> {
    scoped(connection)
        .await
        .map(|scoped| Arc::clone(scoped.documents()))
}

/// The module's scope of the host's storage, when the host serves one.
async fn scoped(connection: Connection) -> Option<tinystoragedrivers_core::ScopedStorage> {
    let bus = match tinystoragedrivers_busproxy::connect_storage(connection).await {
        Ok(bus) => bus,
        Err(error) => {
            log::debug!("[tinyjuice_module] storage bus unavailable: {error}");
            return None;
        }
    };
    match bus.for_scope(&tinystoragedrivers_busproxy::module_scope_label()) {
        Ok(scoped) => Some(scoped),
        Err(error) => {
            log::warn!("[tinyjuice_module] storage bus scope refused: {error}");
            None
        }
    }
}

/// The CCR durable tier on the host's blob port, or `None` when the host
/// serves no storage (or cannot expire blobs for `ttl`).
pub(crate) async fn durable_tier(
    connection: Connection,
    ttl: Option<Duration>,
) -> Option<Arc<dyn CcrStore>> {
    tier_on(blobs(connection).await?, ttl).await
}

/// The CCR durable tier on `blobs`, or `None` when the store cannot be built
/// (for example a handle that cannot expire blobs for `ttl`).
async fn tier_on(blobs: Arc<dyn BlobStore>, ttl: Option<Duration>) -> Option<Arc<dyn CcrStore>> {
    // The store builds a blocking bridge, which must not run on an async worker.
    let opened = tokio::task::spawn_blocking(move || BlobCcrStore::new(blobs, ttl))
        .await
        .map_err(|error| error.to_string())
        .and_then(|opened| opened.map_err(|error| error.to_string()));
    match opened {
        Ok(store) => {
            log::debug!("[tinyjuice_module] ccr durable tier on host blobs ttl={ttl:?}");
            Some(Arc::new(OffWorker(store)))
        }
        Err(error) => {
            log::warn!("[tinyjuice_module] ccr store on host storage unavailable: {error}");
            None
        }
    }
}

/// A [`CcrStore`] whose every call may block on a bus reply, run so the runtime
/// worker that drives the same connection stays free.
///
/// The cache calls the store from async handlers; a blocking bridge on the only
/// worker would wait for a reply that worker must deliver. On a multi-thread
/// runtime `block_in_place` hands the worker's queue to a replacement thread for
/// the duration of the call.
struct OffWorker<S>(S);

impl<S> OffWorker<S> {
    fn off_worker<T>(call: impl FnOnce() -> T) -> T {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(call)
            }
            _ => call(),
        }
    }
}

impl<S: CcrStore> CcrStore for OffWorker<S> {
    fn put(&self, content: &str) -> CcrPutResult {
        Self::off_worker(|| self.0.put(content))
    }

    fn get(&self, token: &str) -> Option<String> {
        Self::off_worker(|| self.0.get(token))
    }

    fn get_range(&self, token: &str, start: usize, end: usize, unit: RangeUnit) -> Option<String> {
        Self::off_worker(|| self.0.get_range(token, start, end, unit))
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
pub(crate) mod tests;
