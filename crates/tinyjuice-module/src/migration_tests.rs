use super::*;
use tinystoragedrivers_core::schema::{BlobTargetRequest, DocumentsTransformRequest, SourceRecord};

fn handler() -> CcrMigrationHandler {
    CcrMigrationHandler {
        schema: tinyjuice_drivers::storage_schema(),
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
}
