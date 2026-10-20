//! The statically linked export is admitted by the same host checks as the
//! dynamic one, without a library file or exported C symbols.
#![cfg(feature = "static-link")]

use tinybus::test_support::wait_until_serving;
use tinybus::{Connection, module::ModuleState};
use tinyjuice_module::{BUS_NAME, linked_module};
use tinystoragedrivers_tinybus::migration::MigrationModuleClient;

#[path = "support/mod.rs"]
mod support;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_module_is_admitted_and_serves_its_bus_name() {
    let fixture = support::start_trusted_bus()
        .await
        .expect("restricted memory broker should start");
    let module = linked_module().expect("generated manifest is valid");
    let info = fixture
        .modules
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
    wait_until_serving(&fixture.client, BUS_NAME, std::time::Duration::from_secs(5))
        .await
        .expect("linked module should become ready");
    let peer = BUS_NAME.parse().expect("module bus name");
    let schema = MigrationModuleClient::discover(&fixture.controller, &peer)
        .await
        .expect("selected controller should discover the linked schema");
    assert_eq!(schema.declaration().owner.as_str(), "juice");

    let outsider = Connection::connect(fixture.bus.connect().await.expect("outsider transport"))
        .await
        .expect("outsider should connect");
    assert!(
        MigrationModuleClient::discover(&outsider, &peer)
            .await
            .is_err(),
        "an unselected controller must not discover the schema"
    );
    fixture.broker_task.abort();
}
