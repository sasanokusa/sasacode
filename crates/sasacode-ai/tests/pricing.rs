//! What a reply costs (`packages/ai/test/pricing.test.ts`).

mod common;

use common::model;
use sasacode_ai::openai_chat::parse_chat_usage;
use sasacode_ai::{ModelInfo, Usage, compute_cost};
use serde_json::{Value, json};

fn priced(price: Option<Value>) -> ModelInfo {
    let mut m = json!({ "id": "m", "provider": "p", "api": "openai-chat", "contextWindow": 1000, "maxOutput": 100 });
    if let Some(price) = price {
        m["price"] = price;
    }
    model(m)
}

/// `toBeCloseTo` with its default precision: within 0.005.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.005
}

#[test]
fn a_model_without_a_price_costs_nothing_computable() {
    assert_eq!(
        compute_cost(
            &priced(None),
            &Usage {
                input: 5,
                output: 7,
                ..Default::default()
            }
        ),
        0.0
    );
}

#[test]
fn a_provider_reported_cost_beats_the_table_estimate() {
    let u = parse_chat_usage(
        &priced(Some(json!({ "input": 1.25, "output": 10 }))),
        &json!({ "prompt_tokens": 1_000_000, "completion_tokens": 100_000, "cost": 0.42 }),
    );
    assert_eq!(u.cost, 0.42);
    assert_eq!(u.input, 1_000_000);
    assert_eq!(u.output, 100_000);
    assert_eq!(u.cache_read, 0);
}

#[test]
fn a_reported_cost_of_zero_means_not_reported_the_estimate_stands() {
    let m = priced(Some(json!({ "input": 1.25, "output": 10 })));
    let estimate = 1.25 + 1.0; // 1M input + 0.1M output at the table's prices
    let zero = parse_chat_usage(
        &m,
        &json!({ "prompt_tokens": 1_000_000, "completion_tokens": 100_000, "cost": 0 }),
    );
    assert!(close(zero.cost, estimate), "{}", zero.cost);
    let absent = parse_chat_usage(&m, &json!({ "prompt_tokens": 1_000_000, "completion_tokens": 100_000 }));
    assert!(close(absent.cost, estimate), "{}", absent.cost);
}

#[test]
fn cached_prompt_tokens_are_parsed_out_and_billed_at_the_cache_rate() {
    let u = parse_chat_usage(
        &priced(Some(json!({ "input": 1.25, "output": 10 }))),
        &json!({ "prompt_tokens": 2_000_000, "completion_tokens": 1, "prompt_tokens_details": { "cached_tokens": 1_000_000 } }),
    );
    assert_eq!(u.input, 1_000_000);
    assert_eq!(u.cache_read, 1_000_000);
    assert!(close(u.cost, 1.25 + 0.125 + 10.0 / 1e6), "{}", u.cost);
}
