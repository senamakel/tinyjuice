//! Persisted savings totals and requests shared with the compression module.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-key (model / compressor) rolled-up savings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavingsBucket {
    /// Number of shrinking compactions; saturates at `u64::MAX`.
    pub events: u64,
    /// Sum of estimated tokens before compaction.
    pub original_tokens: u64,
    /// Sum of estimated tokens after compaction.
    pub compacted_tokens: u64,
    /// Sum of positive pre/post token differences.
    pub tokens_saved: u64,
    /// Estimated dropped-input cost from host prices; saturates at `f64::MAX`.
    pub cost_saved_usd: f64,
}

/// The full savings snapshot returned to callers / the dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavingsAggregate {
    /// Overall totals across every compaction.
    pub total: SavingsBucket,
    /// Breakdown by the model the savings were attributed to.
    pub by_model: HashMap<String, SavingsBucket>,
    /// Breakdown by which compressor produced the saving.
    pub by_compressor: HashMap<String, SavingsBucket>,
}

/// A callback storage endpoint that a host grants for one import invocation.
///
/// The host name must be the unique name of the attested callback connection;
/// `object_root` must identify that invocation's isolated object subtree. The
/// address contains no storage policy or write condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageCallbackAddress {
    /// Unique TinyBus name serving this callback.
    pub host: String,
    /// Per-invocation TinyBus object root served by `host`.
    pub object_root: String,
}

/// A savings snapshot plus a temporary callback destination for adoption.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportSavingsAtRequest {
    /// The legacy savings snapshot to adopt if the destination has no totals.
    pub aggregate: SavingsAggregate,
    /// Host-created callback authority, valid only while the import call runs.
    pub callback: StorageCallbackAddress,
}

/// One compaction attributed to the model that would consume its output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordSavingsRequest {
    /// Model identifier used for the dashboard breakdown and host price lookup.
    pub model: String,
    /// Compressor identifier used for the dashboard breakdown.
    pub compressor: String,
    /// Estimated input tokens before compaction.
    pub original_tokens: u64,
    /// Estimated input tokens after compaction.
    pub compacted_tokens: u64,
    /// Host-resolved price per million input tokens; absent means unpriced.
    pub input_per_mtok_usd: Option<f64>,
}

/// The id of the one document in the `juice.savings` collection.
pub const SAVINGS_DOC_ID: &str = "totals";

/// One bucket as stored in `juice.savings`: bare snake_case, unlike the
/// camelCase wire shape of [`SavingsBucket`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredBucket {
    /// Number of shrinking compactions.
    pub events: u64,
    /// Sum of estimated tokens before compaction.
    pub original_tokens: u64,
    /// Sum of estimated tokens after compaction.
    pub compacted_tokens: u64,
    /// Sum of positive pre/post token differences.
    pub tokens_saved: u64,
    /// Estimated dropped-input cost.
    pub cost_saved_usd: f64,
}

/// The `juice.savings` document body: the whole aggregate, snake_case, with
/// its write time in `_ms`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredSavings {
    /// Overall totals.
    pub total: StoredBucket,
    /// Breakdown by model.
    #[serde(default)]
    pub by_model: HashMap<String, StoredBucket>,
    /// Breakdown by compressor.
    #[serde(default)]
    pub by_compressor: HashMap<String, StoredBucket>,
    /// When this document was last written, epoch milliseconds.
    #[serde(default)]
    pub updated_ms: u64,
}

impl From<&SavingsBucket> for StoredBucket {
    fn from(b: &SavingsBucket) -> Self {
        Self {
            events: b.events,
            original_tokens: b.original_tokens,
            compacted_tokens: b.compacted_tokens,
            tokens_saved: b.tokens_saved,
            cost_saved_usd: b.cost_saved_usd,
        }
    }
}

impl From<StoredBucket> for SavingsBucket {
    fn from(b: StoredBucket) -> Self {
        Self {
            events: b.events,
            original_tokens: b.original_tokens,
            compacted_tokens: b.compacted_tokens,
            tokens_saved: b.tokens_saved,
            cost_saved_usd: b.cost_saved_usd,
        }
    }
}

impl StoredSavings {
    /// The document body for `aggregate`, stamped `updated_ms`.
    #[must_use]
    pub fn from_aggregate(aggregate: &SavingsAggregate, updated_ms: u64) -> Self {
        let map = |m: &HashMap<String, SavingsBucket>| {
            m.iter().map(|(k, v)| (k.clone(), v.into())).collect()
        };
        Self {
            total: (&aggregate.total).into(),
            by_model: map(&aggregate.by_model),
            by_compressor: map(&aggregate.by_compressor),
            updated_ms,
        }
    }

    /// The wire aggregate this body holds.
    #[must_use]
    pub fn into_aggregate(self) -> SavingsAggregate {
        let map =
            |m: HashMap<String, StoredBucket>| m.into_iter().map(|(k, v)| (k, v.into())).collect();
        SavingsAggregate {
            total: self.total.into(),
            by_model: map(self.by_model),
            by_compressor: map(self.by_compressor),
        }
    }
}
