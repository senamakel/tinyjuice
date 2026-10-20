//! Tests for the CCR durable tier on host storage: blobs on the host port,
//! expiry, the in-memory fallback, and single-worker safety.

use super::*;
use tinybus::broker::Broker;
use tinybus::transport::memory::MemoryBus;
use tinyjuice::cache::store::short_hash;
use tinystoragedrivers_busproxy::{PerCallerEndpoint, ServedStorage, serve};
use tinystoragedrivers_client::StorageServer;
use tinystoragedrivers_core::{MemoryStorage, Scope, ScopedStorage};

#[cfg(feature = "static-link")]
#[path = "../tests/support/juice_storage.rs"]
mod governed_fixture;

type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(crate) struct Rig {
    pub(crate) module: Connection,
    pub(crate) backend: MemoryStorage,
    host: Connection,
    _served: Option<ServedStorage>,
}

impl Rig {
    /// Start serving storage on a rig built without it.
    pub(crate) async fn serve_storage(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let bound = self.backend.clone();
        let endpoint = PerCallerEndpoint::new(move |_sender| {
            let scoped: ScopedStorage = bound.for_scope(&Scope::local()).ok()?;
            Some(StorageServer::new(scoped))
        });
        self._served = Some(serve(&self.host, Arc::new(endpoint)).await?);
        Ok(())
    }
}

pub(crate) async fn rig(serve_storage: bool) -> Result<Rig, Box<dyn std::error::Error>> {
    let bus = MemoryBus::new();
    Broker::new().spawn(bus.clone());
    let backend = MemoryStorage::new();
    let host = Connection::connect(bus.connect().await?).await?;
    let served = if serve_storage {
        let bound = backend.clone();
        let endpoint = PerCallerEndpoint::new(move |_sender| {
            let scoped: ScopedStorage = bound.for_scope(&Scope::local()).ok()?;
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

    let host_blobs = Arc::clone(rig.backend.for_scope(&Scope::local())?.blobs());
    let key = format!(
        "{}{}",
        tinyjuice_bus::STORAGE_NEEDS.blob_prefixes[0],
        short_hash(content)
    );
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
    assert!(
        legacy_durable_tier(rig.module.clone(), None)
            .await
            .is_none()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(feature = "static-link")]
async fn descriptor_binding_uses_the_granted_blob_and_document_ports() -> TestResult {
    use tinybus::{
        Connection, broker::Broker, module::ModuleHost, test_support::wait_until_serving,
        transport::memory::MemoryTransport,
    };
    use tinyjuice_bus::wire::InstallRequest;
    use tinystoragedrivers_core::StorageBackend;

    let bus = MemoryBus::new();
    let broker = Broker::new_restricted_module_control();
    let broker_task = broker.spawn(bus.clone());
    let (broker_transport, controller_transport) = MemoryTransport::pair();
    broker.attach_module_controller(Arc::new(broker_transport))?;
    let controller = Connection::connect(Box::new(controller_transport)).await?;
    let modules = ModuleHost::new(broker);
    let client = Connection::connect(bus.connect().await?).await?;
    let loaded = modules
        .attach_linked_with_config(
            crate::linked_module().expect("static TinyJuice module manifest is valid"),
            serde_json::json!({}),
        )
        .expect("the linked module should be admitted");
    wait_until_serving(&client, crate::service::BUS_NAME, Duration::from_secs(5))
        .await
        .expect("the linked module should be ready");
    let peer = client
        .name_owner(crate::service::BUS_NAME)
        .await?
        .ok_or("the ready module has a peer identity")?;

    let governed = governed_fixture::JuiceStorage::attach(&modules, &controller)
        .await
        .unwrap();
    let (lease, descriptor) = governed.lease("profile-a", &peer).await.unwrap();
    modules
        .reinitialize(
            &loaded.name,
            serde_json::json!({"storage_descriptor": descriptor}),
        )
        .await?;
    let proxy = client.proxy(
        crate::service::BUS_NAME,
        crate::service::OBJECT_PATH,
        crate::service::BUS_NAME,
    )?;
    proxy
        .call::<()>(
            "Install",
            (InstallRequest {
                options: tinyjuice::types::CompressOptions {
                    min_bytes_to_compress: 64,
                    ccr_min_tokens: 16,
                    ..Default::default()
                },
                max_cache_entries: 1,
                max_cache_bytes: 1,
                ccr_ttl_secs: Some(600),
                ccr_durable: true,
            },),
        )
        .await?;

    let original = (0..200)
        .map(|index| {
            if index == 137 {
                format!("ERROR request failed id={index}\n")
            } else {
                format!("INFO request completed id={index}\n")
            }
        })
        .collect::<String>();
    let output: tinyjuice::types::CompressedOutput = proxy
        .call(
            "Compress",
            (
                original.clone(),
                tinyjuice::types::ContentHint {
                    explicit: Some(tinyjuice::types::ContentKind::Log),
                    ..Default::default()
                },
            ),
        )
        .await?;
    let token = output
        .ccr_token
        .ok_or("the durable port retains the original")?;
    let scoped = governed
        .backend
        .for_scope(&Scope::new("profile-a")?)
        .unwrap();
    let key = format!("{}{}", tinyjuice_bus::STORAGE_NEEDS.blob_prefixes[0], token);
    assert!(scoped.blobs().get(&key).await?.is_some());
    assert!(governed.revoke(&lease).await);
    let retrieved: Option<String> = proxy.call("Retrieve", (token,)).await?;
    assert_eq!(retrieved, None, "revoked durable reads fail closed");

    governed.shutdown().await?;
    broker_task.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_descriptor_does_not_fall_back_to_a_live_legacy_storage_endpoint() -> TestResult
{
    let rig = rig(true).await?;
    let descriptor: tinystoragedrivers_busproxy::governed::StorageDescriptor =
        serde_json::from_value(serde_json::json!({
            "service_peer": ":1.999",
            "registration_id": 7,
            "object_root": "/org/tinystoragedrivers/Storage/Leases/7"
        }))?;
    assert!(
        tinystoragedrivers_busproxy::connect_storage(rig.module.clone())
            .await
            .is_ok(),
        "the compatibility endpoint is available in this fixture"
    );
    assert!(
        bind(rig.module.clone(), Some(descriptor)).await.is_err(),
        "a missing descriptor owner must fail instead of using the compatibility endpoint"
    );
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
    let rig = rig(true).await?;
    let storage = crate::storage::bind(rig.module.clone(), None)
        .await?
        .ok_or("host storage")?;
    let service = Compression::new(Some(rig.module.clone()), Some(storage), false);
    let host_blobs = Arc::clone(rig.backend.for_scope(&Scope::local())?.blobs());

    let mut install = install_request(true);
    install.options.min_bytes_to_compress = 32;
    install.options.ccr_min_tokens = 1;
    install.max_cache_bytes = 1;
    service.install(install).await?;
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
            tinyjuice::types::ContentHint {
                explicit: Some(tinyjuice::types::ContentKind::Log),
                ..Default::default()
            },
        )
        .await?;
    let token = output.ccr_token.ok_or("offload was not retained")?;
    let key = format!("{}{token}", tinyjuice_bus::STORAGE_NEEDS.blob_prefixes[0]);
    assert!(
        host_blobs.get(&key).await?.is_some(),
        "offload mirrored to host"
    );

    // Switched off: new originals stay in memory only.
    let mut disable = install_request(false);
    disable.options.min_bytes_to_compress = 32;
    disable.options.ccr_min_tokens = 1;
    service.install(disable).await?;
    let memory_only = (0..200)
        .map(|index| {
            if index == 137 {
                format!("ERROR request failed after reset id={index}\n")
            } else {
                format!("INFO request completed after reset id={index}\n")
            }
        })
        .collect::<String>();
    let output = service
        .compress(
            memory_only,
            tinyjuice::types::ContentHint {
                explicit: Some(tinyjuice::types::ContentKind::Log),
                ..Default::default()
            },
        )
        .await?;
    assert!(output.applied);
    let token = output
        .ccr_token
        .ok_or("memory cache should retain the original")?;
    let key = format!("{}{token}", tinyjuice_bus::STORAGE_NEEDS.blob_prefixes[0]);
    assert!(
        host_blobs.get(&key).await?.is_none(),
        "nothing written to the durable tier"
    );
    assert!(host_blobs.get(&key).await?.is_none(), "nothing written");
    Ok(())
}

/// A blob handle that reports no optional abilities (no expiry), over memory.
#[derive(Debug)]
struct NoExpiry(Arc<dyn BlobStore>);

#[async_trait::async_trait]
impl BlobStore for NoExpiry {
    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: Option<&str>,
    ) -> tinystoragedrivers_core::Result<tinystoragedrivers_core::BlobMeta> {
        self.0.put(key, bytes, content_type).await
    }

    async fn get(
        &self,
        key: &str,
    ) -> tinystoragedrivers_core::Result<Option<tinystoragedrivers_core::Blob>> {
        self.0.get(key).await
    }

    async fn get_range(
        &self,
        key: &str,
        range: std::ops::Range<u64>,
    ) -> tinystoragedrivers_core::Result<Option<Vec<u8>>> {
        self.0.get_range(key, range).await
    }

    async fn head(
        &self,
        key: &str,
    ) -> tinystoragedrivers_core::Result<Option<tinystoragedrivers_core::BlobMeta>> {
        self.0.head(key).await
    }

    async fn delete(&self, key: &str) -> tinystoragedrivers_core::Result<bool> {
        self.0.delete(key).await
    }

    async fn list(
        &self,
        prefix: &str,
    ) -> tinystoragedrivers_core::Result<Vec<tinystoragedrivers_core::BlobMeta>> {
        self.0.list(prefix).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handle_that_cannot_expire_blobs_gets_no_ttl_tier_but_a_keep_forever_one() -> TestResult {
    let backend = MemoryStorage::new();
    let blobs: Arc<dyn BlobStore> = Arc::new(NoExpiry(Arc::clone(
        backend.for_scope(&Scope::local())?.blobs(),
    )));
    assert!(
        tier_on(Arc::clone(&blobs), Some(Duration::from_secs(60)))
            .await
            .is_none()
    );
    assert!(tier_on(blobs, None).await.is_some());
    Ok(())
}

/// Off a multi-thread runtime there is no worker to hand off, so the call runs
/// in place.
#[tokio::test(flavor = "current_thread")]
async fn store_calls_run_in_place_on_a_current_thread_runtime() {
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
    assert!(store.put("x").retained());
    assert_eq!(store.get("t").as_deref(), Some("fixed"));
    assert_eq!(
        store.get_range("t", 0, 1, RangeUnit::Lines).as_deref(),
        Some("range")
    );
}

#[test]
fn the_module_grant_is_its_schema_at_module_scope() {
    use tinystoragedrivers_core::schema::{Item, ScopeClass};
    let schema = crate::storage_schema();
    let grant: Vec<(&str, ScopeClass)> = schema
        .items
        .iter()
        .map(|item| match item {
            Item::Blobs { prefix, scope } => (prefix.as_str(), *scope),
            Item::Collection { name, scope, .. } => (name.as_str(), *scope),
            other => panic!("the module stores blobs and collections only, got {other:?}"),
        })
        .collect();
    assert_eq!(
        grant,
        [
            ("juice/ccr/", ScopeClass::Module),
            ("juice.savings", ScopeClass::Module)
        ]
    );
}
