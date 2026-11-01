use super::route_ep_mismatch;
use crate::settings::PersistedRecognitionRoute;

fn dml_route(adapter_index: i32) -> PersistedRecognitionRoute {
    PersistedRecognitionRoute {
        id: format!("dml-{adapter_index}"),
        encoder_compute: "dml".into(),
        decoder_compute: "dml".into(),
        provider: "dml".into(),
        adapter_index: Some(adapter_index),
        adapter_name: Some("GPU".into()),
    }
}

#[test]
fn no_mismatch_when_no_persisted_route() {
    assert_eq!(route_ep_mismatch("cpu", None, None), None);
}

#[test]
fn no_mismatch_when_dml_adapter_matches() {
    let route = dml_route(0);
    assert_eq!(route_ep_mismatch("dml", Some(0), Some(&route)), None);
}

#[test]
fn mismatch_when_loaded_cpu_but_persisted_dml() {
    let route = dml_route(0);
    assert_eq!(
        route_ep_mismatch("cpu", None, Some(&route)),
        Some("dml-0".to_string())
    );
}

#[test]
fn mismatch_when_loaded_different_dml_adapter() {
    let route = dml_route(0);
    assert_eq!(
        route_ep_mismatch("dml", Some(1), Some(&route)),
        Some("dml-0".to_string())
    );
}
