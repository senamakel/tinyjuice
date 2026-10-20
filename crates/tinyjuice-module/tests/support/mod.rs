use std::sync::Arc;

use tinybus::{
    Connection,
    broker::Broker,
    module::ModuleHost,
    transport::memory::{MemoryBus, MemoryTransport},
};
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
