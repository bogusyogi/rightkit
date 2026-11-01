//! Service lifecycle black-box journeys for the documented daemon contracts.

use crate::support::e2e_api::Rig;

#[test]
fn dl010_idle_runtime_reports_passivation_eviction_and_drain_fence() {
    let rig = Rig::start(vec![]);
    let (status, before) = rig.get("/runtime/resource-metrics");
    assert_eq!(status, 200, "{before}");
    let idle = before.get("idle").or_else(|| before.get("passivation"));
    assert!(
        idle.is_some(),
        "runtime exports idle counters needed to measure reclaim: {before}"
    );
    let idle = idle.unwrap();
    assert!(
        idle["passivations"].is_number(),
        "passivation counter: {idle}"
    );
    assert!(idle["evictions"].is_number(), "eviction counter: {idle}");
    assert!(
        idle["mutationDrainFenced"].is_boolean(),
        "mutation drain fence state: {idle}"
    );
}
