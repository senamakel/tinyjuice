use super::*;
use crate::storage::tests::rig;
use tinyjuice_bus::savings::SavingsBucket;
use tinystoragedrivers_api::{DocumentStore, StorageBackend};
use tinystoragedrivers_testkit::{Failing, FaultPlan};
use tinystoragedrivers_types::{ErrorKind, Scope};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn req(original: u64, compacted: u64) -> RecordSavingsRequest {
    RecordSavingsRequest {
        model: "m".into(),
        compressor: "generic".into(),
        original_tokens: original,
        compacted_tokens: compacted,
        input_per_mtok_usd: Some(2.0),
    }
}

async fn stored(
    rig: &crate::storage::tests::Rig,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error>> {
    let docs = Arc::clone(rig.backend.for_scope(&Scope::new("local")?)?.documents());
    Ok(docs
        .get(SAVINGS_COLLECTION, SAVINGS_DOC_ID)
        .await?
        .map(|found| found.doc))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn records_land_as_a_snake_case_document_with_ms_time() -> TestResult {
    let rig = rig(true).await?;
    let savings = Savings::new();
    savings
        .record(Some(&rig.module), &req(2_000_000, 1_000_000))
        .await;
    let body = stored(&rig).await?.ok_or("document written")?;
    assert_eq!(body["total"]["tokens_saved"], 1_000_000);
    assert_eq!(body["by_model"]["m"]["events"], 1);
    assert_eq!(body["by_compressor"]["generic"]["events"], 1);
    assert!(body["updated_ms"].as_u64().unwrap_or(0) > 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_process_resumes_the_stored_totals() -> TestResult {
    let rig = rig(true).await?;
    Savings::new()
        .record(Some(&rig.module), &req(200, 100))
        .await;
    let resumed = Savings::new();
    assert_eq!(
        resumed.stats(Some(&rig.module)).await.total.tokens_saved,
        100
    );
    resumed.record(Some(&rig.module), &req(200, 100)).await;
    assert_eq!(resumed.stats(Some(&rig.module)).await.total.events, 2);
    resumed.reset(Some(&rig.module)).await;
    let body = stored(&rig).await?.ok_or("empty document kept")?;
    assert_eq!(body["total"]["events"], 0);
    assert_eq!(
        Savings::new().stats(Some(&rig.module)).await.total.events,
        0
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_adopts_a_legacy_snapshot_only_when_nothing_was_recorded() -> TestResult {
    let rig = rig(true).await?;
    let legacy = SavingsAggregate {
        total: SavingsBucket {
            events: 3,
            original_tokens: 30,
            compacted_tokens: 10,
            tokens_saved: 20,
            cost_saved_usd: 0.1,
        },
        ..Default::default()
    };
    let savings = Savings::new();
    assert!(savings.import(Some(&rig.module), legacy.clone()).await?);
    assert_eq!(
        savings.stats(Some(&rig.module)).await.total.tokens_saved,
        20
    );
    assert_eq!(stored(&rig).await?.ok_or("written")?["total"]["events"], 3);
    // Importing again must not double count.
    assert!(!savings.import(Some(&rig.module), legacy).await?);
    assert_eq!(savings.stats(Some(&rig.module)).await.total.events, 3);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_storage_host_totals_stay_in_memory() -> TestResult {
    let rig = rig(false).await?;
    let savings = Savings::new();
    savings.record(Some(&rig.module), &req(200, 100)).await;
    assert_eq!(savings.stats(Some(&rig.module)).await.total.events, 1);
    assert!(stored(&rig).await?.is_none());
    let legacy = SavingsAggregate::default();
    assert!(
        savings.import(Some(&rig.module), legacy).await.is_err(),
        "an import cannot be settled while storage is unreadable"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn totals_recorded_before_storage_answers_are_added_to_the_stored_ones() -> TestResult {
    let rig = rig(true).await?;
    Savings::new()
        .record(Some(&rig.module), &req(200, 100))
        .await;
    // A fresh module whose storage endpoint is not up yet.
    let mut late = crate::storage::tests::rig(false).await?;
    late.backend = rig.backend.clone();
    let savings = Savings::with_cooldown(std::time::Duration::ZERO);
    savings.record(Some(&late.module), &req(400, 100)).await;
    assert!(
        stored(&rig)
            .await?
            .is_some_and(|b| b["total"]["events"] == 1)
    );
    late.serve_storage().await?;
    let merged = savings.stats(Some(&late.module)).await;
    assert_eq!(merged.total.events, 2);
    assert_eq!(merged.total.tokens_saved, 100 + 300);
    assert_eq!(stored(&rig).await?.ok_or("written")?["total"]["events"], 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reset_before_storage_answers_discards_the_stored_totals() -> TestResult {
    let rig = rig(true).await?;
    Savings::new()
        .record(Some(&rig.module), &req(200, 100))
        .await;
    let mut late = crate::storage::tests::rig(false).await?;
    late.backend = rig.backend.clone();
    let savings = Savings::with_cooldown(std::time::Duration::ZERO);
    savings.reset(Some(&late.module)).await;
    late.serve_storage().await?;
    assert_eq!(savings.stats(Some(&late.module)).await.total.events, 0);
    assert_eq!(stored(&rig).await?.ok_or("written")?["total"]["events"], 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_without_storage_is_not_asked_again_within_the_cooldown() -> TestResult {
    let mut rig = rig(false).await?;
    let savings = Savings::new();
    savings.record(Some(&rig.module), &req(200, 100)).await;
    rig.serve_storage().await?;
    // Still inside the cooldown: the late port is not looked up yet.
    savings.record(Some(&rig.module), &req(200, 100)).await;
    assert!(stored(&rig).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn an_import_whose_write_fails_is_an_error_leaves_totals_alone_and_can_retry() {
    let backend = tinystoragedrivers_testkit::memory_backend();
    let inner = Arc::clone(
        backend
            .for_scope(&Scope::new("local").unwrap())
            .unwrap()
            .documents(),
    );
    let faults = FaultPlan::new();
    faults.fail_operation_nth("documents.put", 1, ErrorKind::Unavailable);
    let flaky = Arc::new(Failing::with_plan(inner, faults));
    let savings = Savings::new();
    {
        let mut state = savings.state.lock().await;
        state.docs = Some(flaky.clone());
        state.loaded = true;
    }
    let legacy = SavingsAggregate {
        total: SavingsBucket {
            events: 3,
            original_tokens: 30,
            compacted_tokens: 10,
            tokens_saved: 20,
            cost_saved_usd: 0.1,
        },
        ..Default::default()
    };

    let failed = savings.import(None, legacy.clone()).await;
    assert!(failed.is_err(), "a lost write must not look like a handoff");
    assert_eq!(
        savings.stats(None).await.total.events,
        0,
        "totals roll back"
    );

    assert!(savings.import(None, legacy).await.unwrap());
    assert_eq!(savings.stats(None).await.total.tokens_saved, 20);
    let stored = flaky.get(SAVINGS_COLLECTION, SAVINGS_DOC_ID).await.unwrap();
    assert_eq!(stored.unwrap().doc["total"]["events"], 3);
}

#[tokio::test]
async fn request_local_import_refusal_leaves_the_cached_ledger_unchanged() {
    let backend = tinystoragedrivers_testkit::memory_backend();
    let ordinary = backend.for_scope(&Scope::new("ordinary").unwrap()).unwrap();
    let callback = backend.for_scope(&Scope::new("callback").unwrap()).unwrap();
    let faults = FaultPlan::new();
    faults.fail_operation_nth("documents.put", 1, ErrorKind::Unavailable);
    let callback_docs = Arc::new(Failing::with_plan(Arc::clone(callback.documents()), faults));
    let callback_storage = tinystoragedrivers_api::Storage::new(
        callback_docs.clone(),
        Arc::clone(callback.streams()),
        Arc::clone(callback.kv()),
        Arc::clone(callback.blobs()),
        Arc::clone(callback.queues()),
        Arc::clone(callback.secrets()),
    );
    let savings = Savings::new();
    {
        let mut state = savings.state.lock().await;
        state.docs = Some(Arc::clone(ordinary.documents()));
        state.loaded = true;
    }
    let legacy = SavingsAggregate {
        total: SavingsBucket {
            events: 3,
            original_tokens: 30,
            compacted_tokens: 10,
            tokens_saved: 20,
            cost_saved_usd: 0.1,
        },
        ..Default::default()
    };

    let failed = savings.import_at(None, &callback_storage, legacy).await;
    assert!(failed.is_err());
    assert_eq!(savings.stats(None).await.total.events, 0);
    assert!(
        ordinary
            .documents()
            .get(SAVINGS_COLLECTION, SAVINGS_DOC_ID)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        callback
            .documents()
            .get(SAVINGS_COLLECTION, SAVINGS_DOC_ID)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn request_local_import_uses_the_call_storage_without_caching_it() {
    let backend = tinystoragedrivers_testkit::memory_backend();
    let ordinary = backend.for_scope(&Scope::new("local").unwrap()).unwrap();
    let ordinary_docs = Arc::clone(ordinary.documents());
    let callback_docs = Arc::new(Failing::with_plan(
        Arc::clone(&ordinary_docs),
        FaultPlan::new(),
    ));
    let callback_storage = tinystoragedrivers_api::Storage::new(
        callback_docs,
        Arc::clone(ordinary.streams()),
        Arc::clone(ordinary.kv()),
        Arc::clone(ordinary.blobs()),
        Arc::clone(ordinary.queues()),
        Arc::clone(ordinary.secrets()),
    );
    let savings = Savings::new();
    {
        let mut state = savings.state.lock().await;
        state.docs = Some(Arc::clone(&ordinary_docs));
        state.loaded = true;
    }
    let legacy = SavingsAggregate {
        total: SavingsBucket {
            events: 2,
            original_tokens: 18,
            compacted_tokens: 8,
            tokens_saved: 10,
            cost_saved_usd: 0.25,
        },
        ..Default::default()
    };

    assert!(
        savings
            .import_at(None, &callback_storage, legacy)
            .await
            .unwrap()
    );
    assert_eq!(
        ordinary_docs
            .get(SAVINGS_COLLECTION, SAVINGS_DOC_ID)
            .await
            .unwrap()
            .unwrap()
            .doc["total"]["tokens_saved"],
        10
    );
    assert_eq!(savings.stats(None).await.total.events, 2);
    let state = savings.state.lock().await;
    assert!(Arc::ptr_eq(
        state.docs.as_ref().expect("ordinary port remains cached"),
        &ordinary_docs,
    ));
}
