//! TinyJuice's owner-side TSD migration export.

use std::sync::Arc;

use tinybus_module::ModuleSetupContext;
use tinystoragedrivers_core::schema::{
    Schema, Step, StepBody, TransformError, TransformErrorKind, TransformResult,
    declaration_from_schema,
    delivery::{MigrationPayload, MigrationPayloadHandler, MigrationPayloadReply},
};

const CCR_PREFIX_STEP: &str = "ccr_prefix_v2";
const CCR_PREFIX_OPERATION: &str = "tinyjuice.ccr-prefix-v2";
const CCR_PREFIX_OPERATION_VERSION: u32 = 1;

/// Export TinyJuice's existing schema and blob transform to its selected host.
pub(crate) async fn serve(context: &ModuleSetupContext) -> tinybus::Result<()> {
    let schema = tinyjuice_drivers::storage_schema();
    let declaration = declaration_from_schema(&schema, operation_for_step)
        .map_err(|_| tinybus::Error::failed("TinyJuice migration schema unavailable"))?;
    let handler = Arc::new(CcrMigrationHandler { schema });
    tinystoragedrivers_tinybus::migration::serve_module_migration(context, declaration, handler)
        .await
}

fn operation_for_step(step: &Step) -> Option<(String, u32)> {
    (step.id == CCR_PREFIX_STEP).then(|| {
        (
            CCR_PREFIX_OPERATION.to_owned(),
            CCR_PREFIX_OPERATION_VERSION,
        )
    })
}

struct CcrMigrationHandler {
    schema: Schema,
}

impl MigrationPayloadHandler for CcrMigrationHandler {
    fn transform(&self, payload: MigrationPayload) -> MigrationPayloadReply {
        match payload {
            MigrationPayload::Documents(_) => {
                MigrationPayloadReply::Documents(Err(unsupported_operation()))
            }
            MigrationPayload::BlobTarget(request) => {
                MigrationPayloadReply::BlobTarget(self.blob_target(request))
            }
            MigrationPayload::StreamPlan(_) => {
                MigrationPayloadReply::StreamPlan(Err(unsupported_operation()))
            }
            MigrationPayload::StreamEntries(_) => {
                MigrationPayloadReply::StreamEntries(Err(unsupported_operation()))
            }
        }
    }
}

impl CcrMigrationHandler {
    fn blob_target(
        &self,
        request: tinystoragedrivers_core::schema::BlobTargetRequest,
    ) -> TransformResult<Option<String>> {
        if request.operation != CCR_PREFIX_OPERATION {
            return Err(unsupported_operation());
        }
        if request.version != CCR_PREFIX_OPERATION_VERSION {
            return Err(TransformError {
                kind: TransformErrorKind::UnsupportedVersion,
                message: "TinyJuice CCR migration version unsupported".into(),
            });
        }

        let Some(step) = self
            .schema
            .steps
            .iter()
            .find(|step| step.id == CCR_PREFIX_STEP)
        else {
            return Err(unsupported_operation());
        };
        let StepBody::Blobs(transform) = &step.run else {
            return Err(unsupported_operation());
        };

        match transform(request.source, None, Vec::new()) {
            Ok(Some((target, None, bytes))) if bytes.is_empty() => Ok(Some(target)),
            Ok(None) => Ok(None),
            Ok(Some(_)) => Err(TransformError {
                kind: TransformErrorKind::InvalidInput,
                message: "TinyJuice CCR migration only supports key changes".into(),
            }),
            Err(_) => Err(TransformError {
                kind: TransformErrorKind::InvalidInput,
                message: "TinyJuice CCR migration rejected the source".into(),
            }),
        }
    }
}

fn unsupported_operation() -> TransformError {
    TransformError {
        kind: TransformErrorKind::UnsupportedOperation,
        message: "TinyJuice migration operation unsupported".into(),
    }
}

#[cfg(test)]
#[path = "migration_tests.rs"]
mod tests;
