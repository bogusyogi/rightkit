//! The versioned, dated, qualified price catalogue (Orthic-Labs/rightkit#302).

use rightkit_llm::{
    CatalogueError, PriceCatalogue, PriceKind, PriceTable, PriceUnit, Qualification, Usage,
};

fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    }
}

const HOST: &str = r#"{
  "schema": 1, "version": "host-1", "published": "2026-10-17",
  "rows": [
    {"kind": "transcriber", "id": "whisper-large", "effective_from": "2026-01-01",
     "effective_until": "2026-06-30", "qualification": "verified",
     "source": "vendor price page", "checked_on": "2026-01-02",
     "components": [{"unit": "per_audio_minute", "amount": 0.006}]},
    {"kind": "transcriber", "id": "whisper-large", "effective_from": "2026-07-01",
     "qualification": "doc_asserted", "source": "vendor docs",
     "components": [{"unit": "per_audio_minute", "amount": 0.005}]},
    {"kind": "platform_fee", "id": "shop-fee", "effective_from": "2026-01-01",
     "qualification": "estimate", "source": "planning figure",
     "components": [{"unit": "fraction", "amount": 0.029}, {"unit": "per_transaction", "amount": 0.30}]}
  ]
}"#;

#[test]
fn the_bundled_catalogue_loads_and_matches_the_bundled_price_table() {
    let catalogue = PriceCatalogue::bundled();
    assert_eq!(catalogue.schema, 1);
    assert!(!catalogue.version.is_empty());
    let from_catalogue = catalogue
        .llm_table_at(&catalogue.published, Qualification::DocAsserted)
        .unwrap();
    let bundled = PriceTable::bundled();
    assert_eq!(from_catalogue.rows().len(), bundled.rows().len());
    for row in bundled.rows() {
        let other = from_catalogue.lookup(&row.slug).expect("row present");
        assert_eq!(other.input_per_m, row.input_per_m);
        assert_eq!(other.output_per_m, row.output_per_m);
        assert_eq!(other.context_window, row.context_window);
    }
}

#[test]
fn price_at_picks_the_row_for_the_date() {
    let c = PriceCatalogue::from_json(HOST).unwrap();
    let old = c
        .price_at(
            PriceKind::Transcriber,
            "Whisper-Large",
            "2026-03-01",
            Qualification::Estimate,
        )
        .unwrap();
    assert_eq!(old.amount(PriceUnit::PerAudioMinute), Some(0.006));
    let new = c
        .price_at(
            PriceKind::Transcriber,
            "whisper-large",
            "2026-07-01",
            Qualification::Estimate,
        )
        .unwrap();
    assert_eq!(new.amount(PriceUnit::PerAudioMinute), Some(0.005));
}

#[test]
fn every_miss_is_its_own_typed_error() {
    let c = PriceCatalogue::from_json(HOST).unwrap();
    assert!(matches!(
        c.price_at(
            PriceKind::Scraper,
            "x",
            "2026-03-01",
            Qualification::Estimate
        ),
        Err(CatalogueError::Unpriced { .. })
    ));
    assert!(matches!(
        c.price_at(
            PriceKind::Transcriber,
            "whisper-large",
            "2025-12-31",
            Qualification::Estimate
        ),
        Err(CatalogueError::NotInEffect { .. })
    ));
    assert!(matches!(
        c.price_at(
            PriceKind::PlatformFee,
            "shop-fee",
            "2026-03-01",
            Qualification::DocAsserted
        ),
        Err(CatalogueError::BelowQualification {
            have: Qualification::Estimate,
            need: Qualification::DocAsserted,
            ..
        })
    ));
    assert!(matches!(
        c.price_at(
            PriceKind::PlatformFee,
            "shop-fee",
            "2026-3-1",
            Qualification::Estimate
        ),
        Err(CatalogueError::BadDate(_))
    ));
    // A model slug is not matched by prefix.
    let b = PriceCatalogue::bundled();
    assert!(matches!(
        b.price_at(
            PriceKind::Llm,
            "claude-sonnet-5-5",
            "2026-10-17",
            Qualification::Estimate
        ),
        Err(CatalogueError::Unpriced { .. })
    ));
}

#[test]
fn llm_cost_needs_the_components_the_usage_uses() {
    let b = PriceCatalogue::bundled();
    let cost = b
        .llm_cost_at(
            "claude-sonnet-5",
            &usage(1_000_000, 1_000_000),
            "2026-10-17",
            Qualification::DocAsserted,
        )
        .unwrap();
    assert_eq!(cost, 12.0);
    let mut cached = usage(1, 1);
    cached.cache_read_tokens = 10;
    assert!(matches!(
        b.llm_cost_at(
            "claude-sonnet-5",
            &cached,
            "2026-10-17",
            Qualification::DocAsserted
        ),
        Err(CatalogueError::MissingComponent {
            unit: PriceUnit::PerMillionCacheReadTokens,
            ..
        })
    ));
    assert!(matches!(
        b.llm_cost_at(
            "claude-sonnet-5",
            &usage(1, 1),
            "2026-10-17",
            Qualification::Verified
        ),
        Err(CatalogueError::BelowQualification { .. })
    ));
}

#[test]
fn from_json_rejects_malformed_catalogues() {
    let with = |row: &str| {
        format!(r#"{{"schema":1,"version":"v","published":"2026-10-17","rows":[{row}]}}"#)
    };
    let good = r#"{"kind":"scraper","id":"s","effective_from":"2026-01-01","qualification":"verified","source":"src","components":[{"unit":"per_page","amount":0.01}]}"#;
    assert!(PriceCatalogue::from_json(&with(good)).is_ok());
    let bad_date = good.replace("2026-01-01", "2026-02-30");
    assert!(matches!(
        PriceCatalogue::from_json(&with(&bad_date)),
        Err(CatalogueError::InvalidRow { .. })
    ));
    let negative = good.replace("0.01", "-0.01");
    assert!(matches!(
        PriceCatalogue::from_json(&with(&negative)),
        Err(CatalogueError::InvalidRow { .. })
    ));
    let overlap = format!("{good},{good}");
    assert!(matches!(
        PriceCatalogue::from_json(&with(&overlap)),
        Err(CatalogueError::Overlap { .. })
    ));
    let bad_schema = with(good).replace(r#""schema":1"#, r#""schema":2"#);
    assert_eq!(
        PriceCatalogue::from_json(&bad_schema),
        Err(CatalogueError::UnsupportedSchema(2))
    );
}

#[test]
fn merge_adds_rows_and_refuses_a_conflict() {
    let bundled = PriceCatalogue::bundled();
    let host = PriceCatalogue::from_json(HOST).unwrap();
    let merged = bundled.clone().merge(host).unwrap();
    assert!(merged.version.contains('+'));
    assert!(merged
        .price_at(
            PriceKind::PlatformFee,
            "shop-fee",
            "2026-03-01",
            Qualification::Estimate
        )
        .is_ok());
    assert!(merged
        .price_at(
            PriceKind::Llm,
            "claude-sonnet-5",
            "2026-10-17",
            Qualification::Estimate
        )
        .is_ok());

    // Same kind, id and start date, different amount: refused, not replaced.
    let json = bundled
        .to_json()
        .replacen("\"amount\": 2.0", "\"amount\": 3.0", 1);
    let changed = PriceCatalogue::from_json(&json).unwrap();
    assert!(matches!(
        bundled.merge(changed),
        Err(CatalogueError::Conflict { .. })
    ));
}

#[test]
fn json_round_trips() {
    let c = PriceCatalogue::from_json(HOST).unwrap();
    assert_eq!(PriceCatalogue::from_json(&c.to_json()).unwrap(), c);
}
