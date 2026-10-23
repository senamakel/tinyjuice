//! Tests for the CCR durable tier on host storage: blobs on the host port,
//! expiry, the in-memory fallback, and single-worker safety.

use super::*;
use tinybus::broker::Broker;
use tinybus::transport::memory::MemoryBus;
use tinyjuice::cache::store::short_hash;
use tinystoragedrivers_api::{BlobStore, StorageBackend};
use tinystoragedrivers_busproxy::{PerCallerEndpoint, ServedStorage, StorageServer, serve};
use tinystoragedrivers_testkit::MemoryBackend;
use tinystoragedrivers_types::{
    Blob, BlobMeta, Capabilities, Capability, Result as StorageResult, Scope,
};

/// Serializes the tests that `install`, which reconfigures process-global cache
/// state.
pub(crate) static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(crate) struct Rig {
    pub(crate) module: Connection,
    pub(crate) backend: Arc<MemoryBackend>,
    host: Connection,
    _served: Option<ServedStorage>,
}

impl Rig {
    /// Start serving storage on a rig built without it.
    pub(crate) async fn serve_storage(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let bound = Arc::clone(&self.backend);
        let endpoint = PerCallerEndpoint::new(move |_sender| {
            let scoped = bound.for_scope(&Scope::new("local").ok()?).ok()?;
            Some(StorageServer::new(scoped))
        });
        self._served = Some(serve(&self.host, Arc::new(endpoint)).await?);
        Ok(())
    }
}

pub(crate) async fn rig(serve_storage: bool) -> Result<Rig, Box<dyn std::error::Error>> {
    let bus = MemoryBus::new();
    Broker::new().spawn(bus.clone());
    let backend = Arc::new(MemoryBackend::new());
    let host = Connection::connect(bus.connect().await?).await?;
    let served = if serve_storage {
        let bound = Arc::clone(&backend);
        let endpoint = PerCallerEndpoint::new(move |_sender| {
            let scoped = bound.for_scope(&Scope::new("local").ok()?).ok()?;
            Some(StorageServer::new(scoped))
        });
        Some(serve(&host, Arc::new(endpoint)).await?)
    } else {
        None
    };
    let module = Connection::connect(bus.connect().await?).await?;
    Ok(Rig {
        module,
        backend,
        host,
        _served: served,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn originals_land_as_blobs_on_the_host_port() -> TestResult {
    let rig = rig(true).await?;
    let store = durable_tier(rig.module.clone(), Some(Duration::from_secs(600)))
        .await
        .ok_or("host serves storage")?;
    let content = "ccr original on the host blob port\nsecond line";
    let put = store.put(content);
    assert!(put.retained());

    let host_blobs = Arc::clone(rig.backend.for_scope(&Scope::new("local")?)?.blobs());
    let key = format!("{}/{}", tinyjuice_drivers::CCR_PREFIX, short_hash(content));
    let blob = host_blobs.get(&key).await?.ok_or("blob is on the host")?;
    assert_eq!(blob.bytes, content.as_bytes());

    assert_eq!(store.get(put.token()).as_deref(), Some(content));
    assert_eq!(
        store
            .get_range(put.token(), 1, 2, RangeUnit::Lines)
            .as_deref(),
        Some("second line")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_storage_host_there_is_no_durable_tier() -> TestResult {
    let rig = rig(false).await?;
    assert!(durable_tier(rig.module.clone(), None).await.is_none());
    Ok(())
}

/// The module has two workers at most; a store call made directly from async
/// code must not wait on a bus reply only that worker could deliver.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn store_calls_on_the_only_worker_do_not_deadlock() -> TestResult {
    let rig = rig(true).await?;
    let store = durable_tier(rig.module.clone(), None)
        .await
        .ok_or("host serves storage")?;
    tokio::time::timeout(Duration::from_secs(20), async {
        let put = store.put("single worker original");
        assert!(put.retained());
        assert_eq!(
            store.get(put.token()).as_deref(),
            Some("single worker original")
        );
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn durable_calls_fail_closed_on_a_current_thread_runtime() -> TestResult {
    let rig = rig(true).await?;
    let store = durable_tier(rig.module.clone(), None)
        .await
        .ok_or("host serves storage")?;
    let put = store.put("current thread must not block its bus runtime");
    assert!(!put.retained());
    assert!(store.get(put.token()).is_none());

    let blobs = rig.backend.for_scope(&Scope::new("local")?)?;
    assert!(
        blobs
            .blobs()
            .get(&format!(
                "{}/{}",
                tinyjuice_drivers::CCR_PREFIX,
                put.token()
            ))
            .await?
            .is_none()
    );
    Ok(())
}

fn install_request(durable: bool) -> tinyjuice_bus::wire::InstallRequest {
    tinyjuice_bus::wire::InstallRequest {
        options: tinyjuice::types::CompressOptions::default(),
        max_cache_entries: 8,
        max_cache_bytes: 1024 * 1024,
        ccr_ttl_secs: Some(600),
        ccr_durable: durable,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_moves_the_disk_tier_onto_host_blobs_and_off_again() -> TestResult {
    use crate::service::Compression;
    let _install = INSTALL.lock().await;
    let rig = rig(true).await?;
    let service = Compression {
        connection: Some(rig.module.clone()),
    };
    let host_blobs = Arc::clone(rig.backend.for_scope(&Scope::new("local")?)?.blobs());

    service.install(install_request(true)).await?;
    let content = "install wired durable tier fixture original";
    let token = tinyjuice::cache::offload(content);
    let key = format!("{}/{token}", tinyjuice_drivers::CCR_PREFIX);
    assert!(
        host_blobs.get(&key).await?.is_some(),
        "offload mirrored to host"
    );

    // Switched off: new originals stay in memory only.
    service.install(install_request(false)).await?;
    let memory_only = "install cleared durable tier fixture original";
    let token = tinyjuice::cache::offload(memory_only);
    let key = format!("{}/{token}", tinyjuice_drivers::CCR_PREFIX);
    assert!(host_blobs.get(&key).await?.is_none(), "nothing written");
    Ok(())
}

/// A blob handle that reports no optional abilities (no expiry), over memory.
#[derive(Debug)]
struct NoBlobExpiry(Arc<dyn BlobStore>);

#[async_trait::async_trait]
impl BlobStore for NoBlobExpiry {
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities().without(Capability::BlobTtl)
    }

    fn max_key_len(&self) -> usize {
        self.0.max_key_len()
    }

    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: Option<&str>,
    ) -> StorageResult<BlobMeta> {
        self.0.put(key, bytes, content_type).await
    }

    async fn get(&self, key: &str) -> StorageResult<Option<Blob>> {
        self.0.get(key).await
    }

    async fn get_range(
        &self,
        key: &str,
        range: std::ops::Range<u64>,
    ) -> StorageResult<Option<Vec<u8>>> {
        self.0.get_range(key, range).await
    }

    async fn head(&self, key: &str) -> StorageResult<Option<BlobMeta>> {
        self.0.head(key).await
    }

    async fn delete(&self, key: &str) -> StorageResult<bool> {
        self.0.delete(key).await
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<BlobMeta>> {
        self.0.list(prefix).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handle_that_cannot_expire_blobs_gets_no_ttl_tier_but_a_keep_forever_one() -> TestResult {
    let backend = MemoryBackend::new();
    let blobs: Arc<dyn BlobStore> = Arc::new(NoBlobExpiry(Arc::clone(
        backend.for_scope(&Scope::new("local")?)?.blobs(),
    )));
    assert!(
        tier_on(Arc::clone(&blobs), Some(Duration::from_secs(60)))
            .await
            .is_none()
    );
    assert!(tier_on(blobs, None).await.is_some());
    Ok(())
}

/// On a current-thread runtime the synchronous bridge cannot safely wait for
/// the same executor to deliver its reply, so it refuses durable calls.
#[tokio::test(flavor = "current_thread")]
async fn off_worker_refuses_calls_on_a_current_thread_runtime() {
    struct Fixed;
    impl CcrStore for Fixed {
        fn put(&self, content: &str) -> CcrPutResult {
            tinyjuice::cache::store::MemoryCcrStore::default().put(content)
        }
        fn get(&self, _token: &str) -> Option<String> {
            Some("fixed".into())
        }
        fn get_range(&self, _: &str, _: usize, _: usize, _: RangeUnit) -> Option<String> {
            Some("range".into())
        }
    }
    let store = OffWorker(Fixed);
    assert!(!store.put("x").retained());
    assert_eq!(store.get("t"), None);
    assert_eq!(store.get_range("t", 0, 1, RangeUnit::Lines), None);
}

#[test]
fn declares_document_collection_for_host_provisioning() {
    use tinystoragedrivers_types::CollectionSpec;
    assert_eq!(
        crate::collections(),
        vec![CollectionSpec::new("juice.savings")]
    );
}
