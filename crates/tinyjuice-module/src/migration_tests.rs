use super::*;
use tinystoragedrivers_core::schema::{
    BlobTargetRequest, DocumentsTransformRequest, SourceRecord, StreamEntriesRequest,
    StreamPlanRequest,
};

fn handler() -> CcrMigrationHandler {
    CcrMigrationHandler {
        schema: tinyjuice_drivers::storage_schema(),
    }
}

#[test]
fn operation_selector_versions_only_the_existing_ccr_step() {
    let schema = tinyjuice_drivers::storage_schema();
    let ccr_step = schema
        .steps
        .iter()
        .find(|step| step.id == CCR_PREFIX_STEP)
        .expect("the owner's current schema contains the CCR migration step");
    assert_eq!(
        operation_for_step(ccr_step),
        Some((
            CCR_PREFIX_OPERATION.to_owned(),
            CCR_PREFIX_OPERATION_VERSION
        ))
    );

    for step in schema
        .steps
        .iter()
        .filter(|step| step.id != CCR_PREFIX_STEP)
    {
        assert_eq!(operation_for_step(step), None);
    }
}

#[test]
fn ccr_blob_dispatch_uses_the_declared_schema_step() {
    let reply = handler().transform(MigrationPayload::BlobTarget(BlobTargetRequest {
        operation: CCR_PREFIX_OPERATION.into(),
        version: CCR_PREFIX_OPERATION_VERSION,
        source: "tinyjuice_ccr/token".into(),
    }));
    assert_eq!(
        reply,
        MigrationPayloadReply::BlobTarget(Ok(Some("juice/ccr/token".into())))
    );
}

#[test]
fn ccr_blob_dispatch_rejects_unknown_operation_and_version() {
    let handler = handler();
    let unknown_operation = handler.blob_target(BlobTargetRequest {
        operation: "other.owner.operation".into(),
        version: CCR_PREFIX_OPERATION_VERSION,
        source: "tinyjuice_ccr/token".into(),
    });
    assert_eq!(
        unknown_operation.unwrap_err().kind,
        TransformErrorKind::UnsupportedOperation
    );

    let unknown_version = handler.blob_target(BlobTargetRequest {
        operation: CCR_PREFIX_OPERATION.into(),
        version: CCR_PREFIX_OPERATION_VERSION + 1,
        source: "tinyjuice_ccr/token".into(),
    });
    assert_eq!(
        unknown_version.unwrap_err().kind,
        TransformErrorKind::UnsupportedVersion
    );
}

#[test]
fn ccr_export_rejects_other_transform_families() {
    let reply = handler().transform(MigrationPayload::Documents(DocumentsTransformRequest {
        operation: CCR_PREFIX_OPERATION.into(),
        version: CCR_PREFIX_OPERATION_VERSION,
        records: vec![SourceRecord {
            id: "legacy".into(),
            body: serde_json::Value::Null,
            meta: Default::default(),
        }],
    }));
    let MigrationPayloadReply::Documents(Err(error)) = reply else {
        panic!("document transforms are not declared by TinyJuice's CCR schema");
    };
    assert_eq!(error.kind, TransformErrorKind::UnsupportedOperation);

    let reply = handler().transform(MigrationPayload::StreamPlan(StreamPlanRequest {
        operation: CCR_PREFIX_OPERATION.into(),
        version: CCR_PREFIX_OPERATION_VERSION,
        source: "legacy-streams/".into(),
    }));
    assert!(matches!(
        reply,
        MigrationPayloadReply::StreamPlan(Err(TransformError {
            kind: TransformErrorKind::UnsupportedOperation,
            ..
        }))
    ));

    let reply = handler().transform(MigrationPayload::StreamEntries(StreamEntriesRequest {
        operation: CCR_PREFIX_OPERATION.into(),
        version: CCR_PREFIX_OPERATION_VERSION,
        entries: Vec::new(),
    }));
    assert!(matches!(
        reply,
        MigrationPayloadReply::StreamEntries(Err(TransformError {
            kind: TransformErrorKind::UnsupportedOperation,
            ..
        }))
    ));
}
