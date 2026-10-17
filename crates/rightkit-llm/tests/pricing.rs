//! Typed, explicit pricing (Orthic-Labs/rightkit#303).

use rightkit_llm::{
    ChatResponse, ContentBlock, Metering, PriceError, PriceMatch, PriceTable, StopReason, Usage,
};

fn table(json: &str) -> PriceTable {
    PriceTable::from_json(json).unwrap()
}

fn usage() -> Usage {
    Usage {
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    }
}

fn response(model: &str, metering: Metering, known: bool) -> ChatResponse {
    ChatResponse {
        id: String::new(),
        model: model.into(),
        content: vec![ContentBlock::text("x")],
        stop_reason: StopReason::EndTurn,
        usage: usage(),
        usage_known: known,
        metering,
    }
}

const ONE: &str = r#"[{"slug":"anthropic/claude-sonnet-5","input_per_m":2.0,"output_per_m":10.0}]"#;

#[test]
fn a_dated_model_is_not_priced_by_prefix_unless_asked() {
    let t = table(ONE);
    assert!(t.lookup("claude-sonnet-5").is_some());
    assert!(t.lookup("claude-sonnet-5-5").is_none());
    assert_eq!(t.cost("claude-sonnet-5-5", &usage()), None);
    assert!(t.lookup_prefix("claude-sonnet-5-5").is_some());
    assert_eq!(
        t.try_cost("claude-sonnet-5-5", &usage(), PriceMatch::LongestPrefix),
        Ok(12.0)
    );
    assert_eq!(
        t.try_cost("claude-sonnet-5-5", &usage(), PriceMatch::Exact),
        Err(PriceError::Unpriced {
            model: "claude-sonnet-5-5".into()
        })
    );
}

#[test]
fn try_cost_of_blocks_an_unpriced_per_token_response_even_without_usage() {
    let t = table(ONE);
    assert_eq!(
        t.try_cost_of(
            &response("claude-sonnet-5", Metering::PerToken, true),
            PriceMatch::Exact
        ),
        Ok(Some(12.0))
    );
    assert_eq!(
        t.try_cost_of(
            &response("claude-sonnet-5", Metering::PerToken, false),
            PriceMatch::Exact
        ),
        Ok(None)
    );
    assert!(matches!(
        t.try_cost_of(
            &response("mystery", Metering::PerToken, false),
            PriceMatch::Exact
        ),
        Err(PriceError::Unpriced { .. })
    ));
    assert_eq!(
        t.try_cost_of(
            &response("mystery", Metering::Subscription, true),
            PriceMatch::Exact
        ),
        Ok(None)
    );
}

#[test]
fn merge_refuses_a_differing_row_and_accepts_new_and_identical_rows() {
    let base = table(ONE);
    let same = table(ONE);
    assert!(base.clone().merge(same).is_ok());

    let added = table(r#"[{"slug":"x/new-model","input_per_m":1.0,"output_per_m":2.0}]"#);
    let merged = base.clone().merge(added).unwrap();
    assert!(merged.lookup("new-model").is_some());

    let changed = table(r#"[{"slug":"claude-sonnet-5","input_per_m":9.0,"output_per_m":10.0}]"#);
    match base.merge(changed) {
        Err(PriceError::Conflict {
            existing, incoming, ..
        }) => {
            assert_eq!(existing.input_per_m, 2.0);
            assert_eq!(incoming.input_per_m, 9.0);
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn merge_overriding_reports_every_replaced_row() {
    let base = table(ONE);
    let changed = table(r#"[{"slug":"claude-sonnet-5","input_per_m":9.0,"output_per_m":10.0}]"#);
    let (merged, replaced) = base.merge_overriding(changed);
    assert_eq!(merged.lookup("claude-sonnet-5").unwrap().input_per_m, 9.0);
    assert_eq!(replaced.len(), 1);
    assert_eq!(replaced[0].replaced.input_per_m, 2.0);
    assert_eq!(replaced[0].with.input_per_m, 9.0);
}

#[test]
fn from_json_rejects_duplicates_and_bad_prices() {
    let dup = r#"[{"slug":"a/m","input_per_m":1.0,"output_per_m":1.0},{"slug":"b/M","input_per_m":2.0,"output_per_m":2.0}]"#;
    assert!(matches!(
        PriceTable::from_json(dup),
        Err(PriceError::Duplicate { .. })
    ));
    let neg = r#"[{"slug":"m","input_per_m":-1.0,"output_per_m":1.0}]"#;
    assert!(matches!(
        PriceTable::from_json(neg),
        Err(PriceError::Invalid { .. })
    ));
    assert!(matches!(
        PriceTable::from_json("nope"),
        Err(PriceError::Parse(_))
    ));
}

#[test]
fn the_bundled_table_loads() {
    assert!(!PriceTable::bundled().rows().is_empty());
}
