//! Model-attributed savings aggregation, in memory.
//!
//! Durability is the caller's: the module keeps the aggregate in the host's
//! document store (`juice.savings`), loading it with `SavingsLedger::replace`.

use std::sync::Mutex;

pub use tinyjuice_bus::savings::{RecordSavingsRequest, SavingsAggregate, SavingsBucket};

#[derive(Default)]
struct State {
    aggregate: SavingsAggregate,
}

/// A thread-safe ledger scoped to its caller, without a host pricing catalog.
///
/// Counts are estimates supplied by the compressor. Monetary savings use only
/// the host-provided input rate, with unpriced models reporting zero dollars.
#[derive(Default)]
pub struct SavingsLedger {
    state: Mutex<State>,
}

impl SavingsLedger {
    /// Creates an empty in-memory ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces all totals, for restoring persisted state.
    pub fn replace(&self, aggregate: SavingsAggregate) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .aggregate = aggregate;
    }

    /// Adds `other`'s totals to the current ones, for reconciling counts made
    /// before a stored aggregate could be read.
    pub fn absorb(&self, other: &SavingsAggregate) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let aggregate = &mut state.aggregate;
        merge_bucket(&mut aggregate.total, &other.total);
        for (key, bucket) in &other.by_model {
            merge_bucket(aggregate.by_model.entry(key.clone()).or_default(), bucket);
        }
        for (key, bucket) in &other.by_compressor {
            merge_bucket(
                aggregate.by_compressor.entry(key.clone()).or_default(),
                bucket,
            );
        }
    }

    /// Records a strictly smaller compaction into the in-memory totals.
    ///
    /// Equal or expanded output is a no-op. Negative or non-finite input
    /// prices and overflowed per-event costs are unpriced. Bucket totals
    /// saturate at their largest finite representation rather than wrapping.
    pub fn record(&self, request: &RecordSavingsRequest) {
        if request.original_tokens <= request.compacted_tokens {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let cost = request
            .input_per_mtok_usd
            .filter(|price| price.is_finite() && *price >= 0.0)
            .map_or(0.0, |price| {
                (request.original_tokens - request.compacted_tokens) as f64 / 1_000_000.0 * price
            });
        let cost = if cost.is_finite() { cost } else { 0.0 };
        let aggregate = &mut state.aggregate;
        add(&mut aggregate.total, request, cost);
        add(
            aggregate.by_model.entry(request.model.clone()).or_default(),
            request,
            cost,
        );
        add(
            aggregate
                .by_compressor
                .entry(request.compressor.clone())
                .or_default(),
            request,
            cost,
        );
    }

    /// Returns a consistent copy of all current totals.
    #[must_use]
    pub fn stats(&self) -> SavingsAggregate {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .aggregate
            .clone()
    }

    /// Clears the in-memory totals.
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.aggregate = SavingsAggregate::default();
    }
}

/// Add one bucket into another, saturating like [`add`].
fn merge_bucket(into: &mut SavingsBucket, from: &SavingsBucket) {
    into.events = into.events.saturating_add(from.events);
    into.original_tokens = into.original_tokens.saturating_add(from.original_tokens);
    into.compacted_tokens = into.compacted_tokens.saturating_add(from.compacted_tokens);
    into.tokens_saved = into.tokens_saved.saturating_add(from.tokens_saved);
    let cost = into.cost_saved_usd + from.cost_saved_usd;
    into.cost_saved_usd = if cost.is_finite() { cost } else { f64::MAX };
}

/// Accumulate one event without wrapping persisted totals at their limits.
fn add(bucket: &mut SavingsBucket, request: &RecordSavingsRequest, cost: f64) {
    bucket.events = bucket.events.saturating_add(1);
    bucket.original_tokens = bucket
        .original_tokens
        .saturating_add(request.original_tokens);
    bucket.compacted_tokens = bucket
        .compacted_tokens
        .saturating_add(request.compacted_tokens);
    bucket.tokens_saved = bucket
        .tokens_saved
        .saturating_add(request.original_tokens - request.compacted_tokens);
    bucket.cost_saved_usd = (bucket.cost_saved_usd + cost).min(f64::MAX);
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
