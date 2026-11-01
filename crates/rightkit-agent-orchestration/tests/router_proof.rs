//! End-to-end proof-harness test (Lane H2): runs every router-under-test
//! against the held-out corpus through the public crate API only (as an
//! external consumer would), and asserts the sanity ordering the spec
//! requires — deliberately-wrong routers score worse than the real one.

use rightkit_agent_orchestration::{
    held_out_corpus, score_router, AlwaysCapableRouter, AlwaysCheapRouter, DispatchRouterAdapter,
    InvertedRouter, ModelTier, RouterUnderTest,
};

#[test]
fn harness_runs_end_to_end_and_reports_accuracy_and_cost() {
    let corpus = held_out_corpus();
    assert!(!corpus.is_empty(), "held-out corpus must not be empty");

    let dispatch_report = score_router(&DispatchRouterAdapter::new(), &corpus);
    assert_eq!(dispatch_report.corpus_size, corpus.len());
    assert!(dispatch_report.accuracy > 0.0 && dispatch_report.accuracy <= 1.0);
    assert!(dispatch_report.total_cost_usd > 0.0);
    assert!(dispatch_report.always_capable_cost_usd > 0.0);
    // The heuristic router must route at least one task to each tier on this
    // mixed corpus, or the accuracy number would be meaningless (it would
    // just equal one label's prevalence).
    let picked_cheap = dispatch_report
        .per_task
        .iter()
        .any(|t| t.routed_tier == ModelTier::Cheap);
    let picked_capable = dispatch_report
        .per_task
        .iter()
        .any(|t| t.routed_tier == ModelTier::Capable);
    assert!(picked_cheap, "expected at least one Cheap routing decision");
    assert!(
        picked_capable,
        "expected at least one Capable routing decision"
    );
}

#[test]
fn sanity_deliberately_wrong_routers_score_worse_than_the_shipped_heuristic() {
    let corpus = held_out_corpus();

    let routers: Vec<(&str, Box<dyn RouterUnderTest>)> = vec![
        ("dispatch", Box::new(DispatchRouterAdapter::new())),
        ("always_capable", Box::new(AlwaysCapableRouter)),
        ("always_cheap", Box::new(AlwaysCheapRouter)),
        ("inverted", Box::new(InvertedRouter::new())),
    ];

    let mut accuracy_by_name = std::collections::HashMap::new();
    for (name, router) in &routers {
        let report = score_router(router.as_ref(), &corpus);
        accuracy_by_name.insert(*name, report.accuracy);
    }

    let dispatch_acc = accuracy_by_name["dispatch"];
    for wrong in ["always_capable", "always_cheap", "inverted"] {
        assert!(
            accuracy_by_name[wrong] < dispatch_acc,
            "{wrong} ({}) should score worse than dispatch ({dispatch_acc})",
            accuracy_by_name[wrong]
        );
    }
}

#[test]
fn cost_delta_is_measured_against_always_capable_floor() {
    let corpus = held_out_corpus();
    let capable_only = score_router(&AlwaysCapableRouter, &corpus);
    let real = score_router(&DispatchRouterAdapter::new(), &corpus);

    // Always-capable is its own floor: zero delta.
    assert!((capable_only.cost_delta_usd).abs() < 1e-9);
    // A router that ever picks Cheap must show a positive delta (savings)
    // vs that floor.
    assert!(real.cost_delta_usd > 0.0);
    assert!((real.always_capable_cost_usd - capable_only.total_cost_usd).abs() < 1e-9);
}
