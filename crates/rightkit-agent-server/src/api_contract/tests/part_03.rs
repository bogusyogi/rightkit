#[test]
fn daemon_api_endpoint_manifest_has_no_duplicate_method_path_pairs() {
    let mut pairs = std::collections::BTreeSet::new();
    for endpoint in DAEMON_API_ENDPOINTS {
        assert!(
            pairs.insert((endpoint.method, endpoint.path)),
            "duplicate endpoint {} {}",
            endpoint.method,
            endpoint.path
        );
    }
}

#[test]
fn daemon_api_endpoint_manifest_does_not_advertise_session_import() {
    for endpoint in DAEMON_API_ENDPOINTS {
        assert_ne!(
            endpoint.path, "/sessions/import",
            "the cross-harness session import route was rejected in \
                 docs/research/competitive-landscape.md A1 and must not \
                 appear in the public endpoint manifest"
        );
    }
}

#[test]
fn daemon_api_endpoint_manifest_count_is_derived_from_group_sums() {
    let expected = super::group_01::ENDPOINT_GROUP_01.len()
        + super::group_02::ENDPOINT_GROUP_02.len()
        + super::group_03::ENDPOINT_GROUP_03.len()
        + super::group_04::ENDPOINT_GROUP_04.len()
        + super::group_05::ENDPOINT_GROUP_05.len()
        + super::group_06::ENDPOINT_GROUP_06.len()
        + super::group_07::ENDPOINT_GROUP_07.len()
        + super::group_08::ENDPOINT_GROUP_08.len()
        + super::group_09::ENDPOINT_GROUP_09.len()
        + super::group_10::ENDPOINT_GROUP_10.len()
        + super::group_11::ENDPOINT_GROUP_11_RUN_CHANGES.len()
        + super::group_12::ENDPOINT_GROUP_12_DESKTOP.len()
        + super::group_12::ENDPOINT_GROUP_12_DESKTOP_TAIL.len()
        + super::group_13::ENDPOINT_GROUP_13_DRIVERS.len()
        + super::group_14::ENDPOINT_GROUP_14_INFERENCE.len();
    assert_eq!(
        DAEMON_API_ENDPOINTS.len(),
        expected,
        "manifest length must equal the sum of per-group sizes; \
             if this fails a group file was added/removed but the const sum \
             in endpoints.rs was not retargeted to it"
    );
    assert!(
        expected > 0,
        "manifest must advertise at least one endpoint"
    );
}
