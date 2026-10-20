use super::*;

fn request(model: &str, original: u64, compacted: u64, price: Option<f64>) -> RecordSavingsRequest {
    RecordSavingsRequest {
        model: model.into(),
        compressor: "smartcrusher".into(),
        original_tokens: original,
        compacted_tokens: compacted,
        input_per_mtok_usd: price,
    }
}

#[test]
fn records_and_aggregates_with_host_input_price() {
    let ledger = SavingsLedger::new();
    ledger.record(&request("priced", 2_000_000, 1_000_000, Some(3.0)));
    let stats = ledger.stats();
    assert_eq!(stats.total.events, 1);
    assert_eq!(stats.total.original_tokens, 2_000_000);
    assert_eq!(stats.total.compacted_tokens, 1_000_000);
    assert_eq!(stats.total.tokens_saved, 1_000_000);
    assert_eq!(stats.total.cost_saved_usd, 3.0);
    assert_eq!(stats.by_model["priced"].cost_saved_usd, 3.0);
    assert_eq!(stats.by_compressor["smartcrusher"].events, 1);
}

#[test]
fn unpriced_models_keep_token_totals_without_guessing_dollars() {
    let ledger = SavingsLedger::new();
    ledger.record(&request("turn-model-x", 2000, 1000, None));
    assert_eq!(ledger.stats().by_model["turn-model-x"].tokens_saved, 1000);
    assert_eq!(ledger.stats().total.cost_saved_usd, 0.0);
}

#[test]
fn non_smaller_compactions_do_not_record_savings() {
    let ledger = SavingsLedger::new();
    ledger.record(&request("m", 100, 100, Some(3.0)));
    ledger.record(&request("m", 50, 100, Some(3.0)));
    assert_eq!(ledger.stats().total.events, 0);
}

#[test]
fn malformed_price_is_unpriced_and_keeps_snapshots_serializable() {
    for price in [f64::NAN, f64::INFINITY, -1.0] {
        let ledger = SavingsLedger::new();
        ledger.record(&request("m", 200, 100, Some(price)));
        assert_eq!(ledger.stats().total.cost_saved_usd, 0.0);
        assert!(serde_json::to_string(&ledger.stats()).is_ok());
    }
}

#[test]
fn replace_restores_totals_and_later_records_accumulate() {
    let source = SavingsLedger::new();
    source.record(&request("m", 2000, 1000, Some(3.0)));
    let restored = SavingsLedger::new();
    restored.replace(source.stats());
    assert_eq!(restored.stats().total.tokens_saved, 1000);
    restored.record(&request("m", 2000, 1000, Some(3.0)));
    assert_eq!(restored.stats().total.events, 2);
    restored.reset();
    assert_eq!(restored.stats().total.events, 0);
}

#[test]
fn absorb_adds_counts_made_before_a_stored_aggregate_was_read() {
    let stored = SavingsLedger::new();
    stored.record(&request("m", 2000, 1000, Some(3.0)));
    let early = SavingsLedger::new();
    early.record(&request("m", 400, 100, Some(3.0)));
    early.record(&request("other", 400, 300, None));
    let merged = SavingsLedger::new();
    merged.replace(stored.stats());
    merged.absorb(&early.stats());
    let aggregate = merged.stats();
    assert_eq!(aggregate.total.events, 3);
    assert_eq!(aggregate.total.tokens_saved, 1400);
    assert_eq!(aggregate.by_model["m"].events, 2);
    assert_eq!(aggregate.by_model["other"].tokens_saved, 100);
}

#[test]
fn the_aggregate_keeps_its_camel_case_wire_shape() {
    let ledger = SavingsLedger::new();
    ledger.record(&request("m", 2000, 1000, Some(3.0)));
    let wire = serde_json::to_value(ledger.stats()).unwrap();
    assert_eq!(wire["total"]["tokensSaved"], 1000);
    assert_eq!(wire["byModel"]["m"]["events"], 1);
    assert!(wire["total"].get("tokens_saved").is_none());
}

#[test]
fn overflowed_cost_calculations_are_unpriced() {
    let ledger = SavingsLedger::new();
    ledger.record(&request("m", 2_000_000, 0, Some(f64::MAX)));
    assert_eq!(ledger.stats().total.tokens_saved, 2_000_000);
    assert_eq!(ledger.stats().total.cost_saved_usd, 0.0);
    let json = serde_json::to_string(&ledger.stats()).unwrap();
    assert!(serde_json::from_str::<SavingsAggregate>(&json).is_ok());
}

#[test]
fn bucket_token_totals_saturate_instead_of_panicking_or_wrapping() {
    let ledger = SavingsLedger::new();
    ledger.record(&request("m", u64::MAX, 0, None));
    ledger.record(&request("m", u64::MAX, 0, None));
    let aggregate = ledger.stats();
    for bucket in [
        &aggregate.total,
        &aggregate.by_model["m"],
        &aggregate.by_compressor["smartcrusher"],
    ] {
        assert_eq!(bucket.original_tokens, u64::MAX);
        assert_eq!(bucket.tokens_saved, u64::MAX);
        assert_eq!(bucket.events, 2);
    }
}

#[test]
fn bucket_dollar_totals_saturate_at_the_largest_finite_value() {
    let ledger = SavingsLedger::new();
    ledger.record(&request("m", 2_000_000, 0, Some(f64::MAX / 2.0)));
    ledger.record(&request("m", 2_000_000, 0, Some(f64::MAX / 2.0)));
    let aggregate = ledger.stats();
    assert_eq!(aggregate.total.cost_saved_usd, f64::MAX);
    let json = serde_json::to_string(&aggregate).unwrap();
    assert!(serde_json::from_str::<SavingsAggregate>(&json).is_ok());
}
