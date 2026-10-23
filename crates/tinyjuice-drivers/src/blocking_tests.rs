use super::*;

#[test]
fn debug_output_hides_runtime_implementation() {
    let bridge = Blocking::new().unwrap();
    let debug = format!("{bridge:?}");

    assert_eq!(debug, "Blocking { .. }");
}

#[test]
fn bridge_rejects_reentrant_storage_calls() {
    let bridge = Blocking::new().unwrap();
    let nested = bridge.clone();

    let nested_result = bridge.run(async move { nested.run(async {}) }).unwrap();

    assert!(
        nested_result
            .unwrap_err()
            .to_string()
            .contains("re-entered")
    );
}

#[test]
fn bridge_reports_calls_when_its_worker_has_stopped() {
    let (jobs, receiver) = mpsc::channel::<Job>();
    drop(receiver);
    let bridge = Blocking { jobs };

    let error = bridge.run(async {}).unwrap_err();

    assert!(error.to_string().contains("storage bridge stopped"));
}

#[test]
fn bridge_reports_a_worker_that_drops_a_call_without_replying() {
    let (jobs, receiver) = mpsc::channel::<Job>();
    thread::spawn(move || drop(receiver.recv().unwrap()));
    let bridge = Blocking { jobs };

    let error = bridge.run(async {}).unwrap_err();

    assert!(error.to_string().contains("storage bridge dropped a call"));
}

#[test]
fn startup_errors_keep_their_source_and_backend_context() {
    let runtime_error = runtime_build_error(std::io::Error::other("runtime resources unavailable"));
    let thread_error = thread_start_error(std::io::Error::other("thread resources unavailable"));

    assert!(
        runtime_error
            .to_string()
            .contains("cannot build the storage bridge runtime")
    );
    assert!(std::error::Error::source(&runtime_error).is_some());
    assert!(
        thread_error
            .to_string()
            .contains("cannot start the storage bridge")
    );
    assert!(std::error::Error::source(&thread_error).is_some());
}
