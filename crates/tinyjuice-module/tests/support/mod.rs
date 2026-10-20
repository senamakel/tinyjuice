use std::sync::Arc;

use async_trait::async_trait;
use tinybus::{
    Connection,
    broker::Broker,
    module::ModuleHost,
    transport::memory::{MemoryBus, MemoryTransport},
};
use tinystoragedrivers_busproxy::{
    AuditEvent, AuditSink, Environment, GovernedEndpointLimits, GovernedStorage, GrantPolicy,
    HostContext, HostFailure, HostStorageContext, ResolvedStorage, StorageDescriptor, StorageGrant,
    StorageLease,
};
use tinystoragedrivers_client::StorageServer;
use tinystoragedrivers_core::{MemoryStorage, Scope, StorageBackend};
use tinystoragedrivers_tinybus::{StorageRegistration, attach_storage};

#[derive(Clone)]
struct Profile(String);

impl HostContext for Profile {}

struct JuiceEnvironment {
    backend: Arc<MemoryStorage>,
}

#[async_trait]
impl Environment<Profile> for JuiceEnvironment {
    async fn resolve(
        &self,
        context: &Profile,
        _module: &tinybus::module::ModuleId,
    ) -> Result<ResolvedStorage, HostFailure> {
        let scope = Scope::new(&context.0).map_err(|_| HostFailure::Denied("invalid-scope"))?;
        let storage = self
            .backend
            .for_scope(&scope)
            .map_err(|_| HostFailure::Denied("storage-unavailable"))?;
        let grant = StorageGrant::from_needs(
            *tinyjuice_bus::STORAGE_NEEDS,
            &GrantPolicy {
                writable: true,
                ..GrantPolicy::default()
            },
        )
        .map_err(|_| HostFailure::Denied("invalid-storage-grant"))?;
        Ok(ResolvedStorage {
            backend_identity: "tinyjuice-module-e2e".into(),
            binding_epoch: 1,
            canonical_scope: context.0.clone(),
            grant,
            server: Arc::new(StorageServer::new(storage)),
        })
    }

    async fn admit_write(
        &self,
        _context: &Profile,
        _module: &tinybus::module::ModuleId,
    ) -> Result<(), HostFailure> {
        Ok(())
    }
}

#[derive(Default)]
struct NoAudit;

impl AuditSink for NoAudit {
    fn record(&self, _event: AuditEvent) {}
}

type JuiceRegistration = StorageRegistration<JuiceEnvironment, NoAudit, Profile>;

/// Current-only storage leases used by the compiled module proof.
pub(super) struct JuiceStorage {
    pub(super) backend: Arc<MemoryStorage>,
    registration: JuiceRegistration,
}

impl JuiceStorage {
    pub(super) async fn attach(
        host: &ModuleHost,
        controller: &Connection,
    ) -> Result<Self, HostFailure> {
        let backend = Arc::new(MemoryStorage::new());
        let endpoint = GovernedStorage::new(
            Arc::new(JuiceEnvironment {
                backend: Arc::clone(&backend),
            }),
            Arc::new(NoAudit),
            GovernedEndpointLimits::default(),
        );
        let registration = attach_storage(
            host,
            controller,
            endpoint,
            tokio::runtime::Handle::current(),
        )
        .await?;
        Ok(Self {
            backend,
            registration,
        })
    }

    pub(super) async fn lease(
        &self,
        profile: &str,
        module_peer: &tinybus::BusName,
    ) -> Result<(StorageLease, StorageDescriptor), HostFailure> {
        self.registration
            .register_lease(
                HostStorageContext::new(Profile(profile.to_owned())),
                module_peer.clone(),
            )
            .await
    }

    pub(super) async fn revoke(&self, lease: &StorageLease) -> bool {
        self.registration.revoke(lease).await
    }

    pub(super) async fn shutdown(self) -> tinybus::Result<()> {
        self.registration.shutdown().await
    }
}

pub(super) struct TrustedBus {
    pub(super) bus: MemoryBus,
    pub(super) modules: ModuleHost,
    pub(super) client: Connection,
    pub(super) controller: Connection,
    pub(super) broker_task: tokio::task::JoinHandle<tinybus::Result<()>>,
}

pub(super) async fn start_trusted_bus() -> tinybus::Result<TrustedBus> {
    let bus = MemoryBus::new();
    let broker = Broker::new_restricted_module_control();
    let broker_task = broker.spawn(bus.clone());
    let (broker_transport, controller_transport) = MemoryTransport::pair();
    broker.attach_module_controller(Arc::new(broker_transport))?;
    let controller = Connection::connect(Box::new(controller_transport)).await?;
    let modules = ModuleHost::new(broker);
    let client = Connection::connect(bus.connect().await?).await?;

    Ok(TrustedBus {
        bus,
        modules,
        client,
        controller,
        broker_task,
    })
}
