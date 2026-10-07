//! Platform-independent contract checks (the e2e suite is Unix-only): role
//! ranking and wire spellings, engine selector serialization, record round
//! trips, and host planning against an empty suite root.

use rightkit_speech_client::*;
use serde_json::json;

#[test]
fn role_wire_names_round_trip_and_rank_orders_takeover() {
    for role in [
        HostRole::HeardRightShell,
        HostRole::HeardRightHeadless,
        HostRole::ScrapeRight,
        HostRole::CutRight,
    ] {
        assert_eq!(HostRole::from_wire(role.as_str()), Some(role));
        assert_eq!(serde_json::to_value(role).unwrap(), json!(role.as_str()));
        assert_eq!(HostRole::rank_table()[role.as_str()], role.rank());
    }
    assert!(HostRole::HeardRightShell.outranks(HostRole::CutRight));
    assert!(!HostRole::CutRight.outranks(HostRole::CutRight));
    assert_eq!(HostRole::parse_env(" CutRight "), Some(HostRole::CutRight));
    assert_eq!(HostRole::parse_env("bogus"), None);
}

#[test]
fn engine_selector_serializes_as_wire_string_and_normalizes_input() {
    assert_eq!(
        serde_json::to_value(EngineSelector::whisper()).unwrap(),
        json!("whisper")
    );
    assert_eq!(
        serde_json::to_value(EngineSelector::Auto).unwrap(),
        json!("auto")
    );
    let parsed: EngineSelector = serde_json::from_value(json!("  Parakeet ")).unwrap();
    assert_eq!(parsed, EngineSelector::parakeet());
    let auto: EngineSelector = serde_json::from_value(json!("")).unwrap();
    assert_eq!(auto, EngineSelector::Auto);
}

#[test]
fn service_record_and_takeover_request_round_trip_through_json() {
    let record = ServiceRecord {
        protocol_major: SERVICE_PROTOCOL_MAJOR,
        protocol_minor: SERVICE_PROTOCOL_MINOR_ENGINES,
        role: HostRole::ScrapeRight,
        pid: 4242,
        endpoint: "pipe-or-socket".into(),
        engine_version: "1.2.3".into(),
    };
    let back: ServiceRecord =
        serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
    assert_eq!(back, record);
    assert_eq!(
        serde_json::to_value(&record).unwrap()["role"],
        "scrape_right"
    );

    let ask = TakeoverRequest {
        role: HostRole::HeardRightShell,
        pid: 7,
        requested_at_ms: 99,
    };
    let back: TakeoverRequest =
        serde_json::from_value(serde_json::to_value(&ask).unwrap()).unwrap();
    assert_eq!(back, ask);
}

#[test]
fn plan_host_with_no_live_host_self_hosts_and_takeover_needs_a_lower_ranked_other_pid() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        plan_host(dir.path(), HostRole::HeardRightShell),
        HostPlan::HostMyself
    );
    // No installed HeardRight on record: lower-priority roles also host themselves.
    assert_eq!(
        plan_host(dir.path(), HostRole::CutRight),
        HostPlan::HostMyself
    );

    let record = ServiceRecord {
        protocol_major: 1,
        protocol_minor: 0,
        role: HostRole::CutRight,
        pid: std::process::id().wrapping_add(1),
        endpoint: String::new(),
        engine_version: String::new(),
    };
    assert!(should_request_takeover(
        HostRole::HeardRightShell,
        Some(&record)
    ));
    assert!(!should_request_takeover(HostRole::CutRight, Some(&record)));
    assert!(!should_request_takeover(HostRole::HeardRightShell, None));
}

#[test]
fn verbatim_round_trips_through_the_request_and_capabilities() {
    use rightkit_speech_client::{FileOptions, ServiceEnvelope, CAPABILITY_VERBATIM};
    let opts = FileOptions {
        verbatim: true,
        ..Default::default()
    };
    let req = ServiceEnvelope::file_request_with("r", "t", "/a.wav".into(), &opts);
    assert!(req.file_options().verbatim, "a host reads the flag back");
    let plain =
        ServiceEnvelope::file_request_with("r", "t", "/a.wav".into(), &FileOptions::default());
    assert!(!plain.file_options().verbatim);
    assert!(plain.payload.as_ref().unwrap().get("verbatim").is_none());
    let caps = ServiceEnvelope::capabilities_full("r", "t", &[], true)
        .capabilities_info()
        .unwrap();
    assert!(caps.supports_verbatim() && caps.supports_file_transcription());
    assert!(caps.capabilities.iter().any(|c| c == CAPABILITY_VERBATIM));
    let caps = ServiceEnvelope::capabilities_with_engines("r", "t", &[])
        .capabilities_info()
        .unwrap();
    assert!(!caps.supports_verbatim());
}
