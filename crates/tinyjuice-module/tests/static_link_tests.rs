//! The statically linked export is admitted by the same host checks as the
//! dynamic one, without a library file or exported C symbols.
#![cfg(feature = "static-link")]

use tinybus::module::ModuleState;
use tinybus::test_support::wait_until_serving;
use tinyjuice_module::{BUS_NAME, linked_module};

#[path = "support/mod.rs"]
mod support;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_module_is_admitted_and_serves_its_bus_name() {
    let support::TrustedBus {
        modules: host,
        client,
        broker_task,
        ..
    } = support::start_trusted_bus()
        .await
        .expect("restricted memory broker should start");
    let module = linked_module().expect("generated manifest is valid");
    let info = host
        .attach_linked_with_config(module, serde_json::json!({}))
        .expect("linked module should be admitted");
    assert_eq!(info.manifest.bus_name.as_str(), BUS_NAME);
    assert!(matches!(
        info.state,
        ModuleState::Resolved
            | ModuleState::Initializing
            | ModuleState::Ready
            | ModuleState::Serving
    ));
    let methods: Vec<&str> = info
        .manifest
        .provides
        .iter()
        .flat_map(|interface| interface.methods.iter())
        .map(tinybus::MemberName::as_str)
        .collect();
    assert_eq!(methods, tinyjuice_bus::METHODS);
    wait_until_serving(&client, BUS_NAME, std::time::Duration::from_secs(5))
        .await
        .expect("linked module should become ready");
    broker_task.abort();
}
