#![cfg(feature = "rightapps")]
use rightkit_models::{rightapps::*, *};
#[path = "support/model_http_mock.rs"]
mod model_http_mock;
use model_http_mock::{MockServer, Reply, Request};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/runtime-envelope.json")).unwrap()
}
fn public_key() -> [u8; 32] {
    serde_json::from_value(vectors()["publicKey"].clone()).unwrap()
}
fn body() -> Vec<u8> {
    (0..2_000_000).map(|i| (i % 251) as u8).collect()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn target() -> RuntimeArtifactTarget {
    RuntimeArtifactTarget::new("darwin", "aarch64")
}
fn pointer() -> String {
    vectors()["envelope"]["manifest"]["pointerKey"]
        .as_str()
        .unwrap()
        .into()
}

fn artifact_envelope(id: &str) -> Value {
    vectors()["artifacts"][id].clone()
}

#[test]
fn javascript_signer_vector_verifies_and_rejects_tampering() {
    let client = RuntimeArtifactClient::new("https://api.example.invalid", public_key()).unwrap();
    let value = vectors();
    let m = client.verify_envelope(&value["envelope"]).unwrap().manifest;
    assert_eq!(m.target.os, "any");
    assert_eq!(m.versions.provenance, "测试-é-🎙");
    for path in [
        "/manifest/object/sizeBytes",
        "/manifest/versions/model",
        "/signature/value",
        "/signature/keyId",
        "/signature/algorithm",
    ] {
        let mut tampered = value["envelope"].clone();
        let at = tampered.pointer_mut(path).unwrap();
        *at = if at.is_number() {
            json!(99)
        } else {
            json!("tampered")
        };
        assert!(client.verify_envelope(&tampered).is_err(), "{path}");
    }
    let wrong_key = RuntimeArtifactClient::new(
        "https://api.example.invalid",
        [
            115, 7, 50, 136, 83, 85, 196, 64, 12, 17, 85, 146, 164, 44, 145, 184, 176, 39, 125, 70,
            12, 178, 181, 213, 156, 30, 40, 21, 167, 77, 93, 34,
        ],
    )
    .unwrap();
    assert!(matches!(
        wrong_key.verify_envelope(&value["envelope"]),
        Err(RuntimeArtifactError::SignatureMismatch)
    ));
    let mut extra = value["envelope"].clone();
    extra["manifest"]["extra"] = json!(1);
    assert!(matches!(
        client.verify_envelope(&extra),
        Err(RuntimeArtifactError::InvalidManifest)
    ));
    for field in ["pointerKey", "object", "provenance", "target", "versions"] {
        let mut missing = value["envelope"].clone();
        missing["manifest"].as_object_mut().unwrap().remove(field);
        assert!(client.verify_envelope(&missing).is_err());
    }
}

#[test]
fn artifact_ids_are_signed_independently_and_legacy_serialization_is_stable() {
    let client = RuntimeArtifactClient::new("https://api.example.invalid", public_key()).unwrap();
    let value = vectors();
    let legacy = client.verify_envelope(&value["envelope"]).unwrap();
    assert!(serde_json::to_value(&legacy.manifest)
        .unwrap()
        .get("artifactId")
        .is_none());

    let ids = [
        "qwen3-base",
        "qwen3-chat",
        "qwen3-code",
        "qwen3-embed",
        "qwen3-instruct",
        "qwen3-vision",
    ];
    for (index, id) in ids.into_iter().enumerate() {
        let envelope = artifact_envelope(id);
        let manifest = client.verify_envelope(&envelope).unwrap().manifest;
        assert_eq!(manifest.artifact_id.as_deref(), Some(id));
        assert_eq!(manifest.object.filename, format!("qwen-{}.gguf", index + 1));
        let expected_pointer =
            format!("genright/runtime-artifacts/tts-model/{id}/any/any/current/manifest.json");
        assert_eq!(
            manifest.pointer_key.as_deref(),
            Some(expected_pointer.as_str())
        );
    }
}

#[test]
fn artifact_id_slug_and_null_validation_is_strict() {
    let client = RuntimeArtifactClient::new("https://api.example.invalid", public_key()).unwrap();
    for invalid in [
        "",
        "Upper",
        "bad_id",
        "bad--id",
        "-leading",
        "trailing-",
        "a/b",
    ] {
        let mut value = artifact_envelope("qwen3-base");
        value["manifest"]["artifactId"] = json!(invalid);
        assert!(
            matches!(
                client.verify_envelope(&value),
                Err(RuntimeArtifactError::InvalidManifest)
            ),
            "{invalid:?}"
        );
    }
    let mut too_long = artifact_envelope("qwen3-base");
    too_long["manifest"]["artifactId"] = json!("a".repeat(129));
    assert!(matches!(
        client.verify_envelope(&too_long),
        Err(RuntimeArtifactError::InvalidManifest)
    ));
    let mut null = artifact_envelope("qwen3-base");
    null["manifest"]["artifactId"] = Value::Null;
    assert!(matches!(
        client.verify_envelope(&null),
        Err(RuntimeArtifactError::InvalidManifest)
    ));
}

#[test]
fn artifact_id_response_binding_rejects_substitution() {
    let mock = MockServer::start_with(|req, base| {
        if req.is("POST", "/v1/apps/genright/runtime-artifacts/resolve") {
            assert_eq!(req.json()["artifactId"], "qwen3-base");
            assert!(req.json().get("artifact").is_none());
            return Some(Reply::json(json!({
                "envelope": artifact_envelope("qwen3-chat"),
                "downloadUrl": format!("{base}/object"),
                "expiresAt": now() + 299
            })));
        }
        None
    })
    .unwrap();
    let client = RuntimeArtifactClient::new(&mock.base, public_key()).unwrap();
    assert!(matches!(
        client.resolve_by_artifact_id("genright", "qwen3-base", "test-license", &target()),
        Err(RuntimeArtifactError::InvalidManifest)
    ));
}

#[test]
fn uuid_shaped_artifact_id_uses_explicit_wire_selector() {
    let id = "123e4567-e89b-12d3-a456-426614174000";
    let mock = MockServer::start_with(move |req, base| {
        if req.is("POST", "/v1/apps/genright/runtime-artifacts/resolve") {
            assert_eq!(req.json()["artifactId"], id);
            assert!(req.json().get("artifact").is_none());
            return Some(Reply::json(json!({
                "envelope": vectors()["uuidArtifact"],
                "downloadUrl": format!("{base}/object"),
                "expiresAt": now() + 299
            })));
        }
        None
    })
    .unwrap();
    let client = RuntimeArtifactClient::new(&mock.base, public_key()).unwrap();
    let resolved = client
        .resolve_by_artifact_id("genright", id, "test-license", &target())
        .unwrap();
    assert_eq!(
        resolved.envelope().manifest.artifact_id.as_deref(),
        Some(id)
    );
}

fn mock_download(bytes: Vec<u8>) -> MockServer {
    let grant = Arc::new(AtomicUsize::new(0));
    MockServer::start_with(move |req: &Request, base: &str| {
        if req.is("POST", "/v1/apps/genright/runtime-artifacts/resolve") {
            assert_eq!(req.header("authorization"), Some("Bearer test-license"));
            assert_eq!(req.json()["artifact"], pointer());
            assert_eq!(req.json()["target"], json!({"os":"darwin","arch":"aarch64"}));
            let n = grant.fetch_add(1, Ordering::SeqCst);
            return Some(Reply::json(json!({"envelope":vectors()["envelope"], "downloadUrl":format!("{base}/object?token=grant{n}"),"expiresAt":now()+299})));
        }
        if req.is("GET", "/object") {
            assert!(req.header("authorization").is_none(), "license must never reach download origin");
            return Some(Reply::bytes("application/octet-stream", bytes.clone()).with_header("ETag", "\"immutable-test-object\""));
        }
        None
    }).unwrap()
}

#[test]
fn resolve_pause_refresh_resume_and_atomic_install() {
    let mock = mock_download(body());
    let client = RuntimeArtifactClient::new(&mock.base, public_key()).unwrap();
    let resolved = client
        .resolve("genright", &pointer(), "test-license", &target())
        .unwrap();
    assert!(!format!("{resolved:?}").contains("grant0"));
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path(), "genright"));
    let options = InstallOptions::default();
    let pause = options.pause.clone();
    let error = client
        .install(&resolved, &store, "qwen-talker", &options, |event| {
            if let Event::Progress { file_done, .. } = event {
                if file_done > 500_000 {
                    pause.store(true, Ordering::SeqCst);
                }
            }
        })
        .unwrap_err();
    assert!(matches!(
        error,
        RuntimeArtifactError::Store(Error {
            code: ErrorCode::Paused,
            ..
        })
    ));
    assert!(store.active_dir("qwen-talker").is_none());
    let refreshed = client
        .resolve("genright", &pointer(), "test-license", &target())
        .unwrap();
    assert_ne!(resolved.download_url(), refreshed.download_url());
    let out = client
        .install(
            &refreshed,
            &store,
            "qwen-talker",
            &InstallOptions::default(),
            |_| {},
        )
        .unwrap();
    assert_eq!(std::fs::read(out.dir.join("qwen.gguf")).unwrap(), body());
    assert!(store.verify("qwen-talker", &out.key, VerifyMode::Full));
    let downloads: Vec<_> = mock
        .seen()
        .into_iter()
        .filter(|r| r.is("GET", "/object"))
        .collect();
    assert_eq!(downloads.len(), 2);
    assert!(downloads[1].header("range").unwrap().starts_with("bytes="));
    assert_eq!(downloads[1].query_param("token"), Some("grant1"));
    let again = client
        .install(
            &refreshed,
            &store,
            "qwen-talker",
            &InstallOptions::default(),
            |_| {},
        )
        .unwrap();
    assert!(again.already_installed);
    assert_eq!(
        mock.seen()
            .into_iter()
            .filter(|r| r.is("GET", "/object"))
            .count(),
        2
    );
}

#[test]
fn bad_download_never_activates() {
    let mock = mock_download(vec![0; 2_000_000]);
    let client = RuntimeArtifactClient::new(&mock.base, public_key()).unwrap();
    let resolved = client
        .resolve("genright", &pointer(), "test-license", &target())
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path(), "genright"));
    assert!(matches!(
        client.install(&resolved, &store, "bad", &InstallOptions::default(), |_| {}),
        Err(RuntimeArtifactError::DigestMismatch)
    ));
    assert!(store.active_dir("bad").is_none());
}

#[test]
fn typed_resolve_errors_and_response_binding() {
    for (status, code) in [
        (403, "NOT_ENTITLED"),
        (401, "TOKEN_EXPIRED"),
        (503, "UNAVAILABLE"),
    ] {
        let mock = MockServer::start_with(move |_, _| {
            Some(Reply::status(status, json!({"error":{"code":code}})))
        })
        .unwrap();
        let client = RuntimeArtifactClient::new(&mock.base, public_key()).unwrap();
        let error = client
            .resolve("genright", &pointer(), "tok", &target())
            .unwrap_err();
        assert!(match status {
            403 => matches!(error, RuntimeArtifactError::NotEntitled),
            401 => matches!(error, RuntimeArtifactError::ExpiredToken),
            _ => matches!(error, RuntimeArtifactError::Network),
        });
    }
    for case in [
        "wrong-app",
        "wrong-pointer",
        "expired-url",
        "long-url",
        "insecure-url",
        "tampered",
    ] {
        let mock = MockServer::start_with(move |_, base| {
            let mut envelope = vectors()["envelope"].clone();
            if case == "tampered" { envelope["manifest"]["versions"]["model"] = json!("evil"); }
            Some(Reply::json(json!({"envelope":envelope, "downloadUrl":if case=="insecure-url" {"http://example.invalid/file".into()} else {format!("{base}/object")},"expiresAt":if case=="expired-url" {now()-1} else if case=="long-url" {now()+1000} else {now()+299}})))
        }).unwrap();
        let client = RuntimeArtifactClient::new(&mock.base, public_key()).unwrap();
        let expected_pointer = pointer();
        assert!(
            client
                .resolve(
                    if case == "wrong-app" {
                        "heardright"
                    } else {
                        "genright"
                    },
                    if case == "wrong-pointer" {
                        "genright/wrong"
                    } else {
                        &expected_pointer
                    },
                    "tok",
                    &target()
                )
                .is_err(),
            "{case}"
        );
    }
}

#[test]
fn exact_windows_target_rejects_mac_and_any_serves_both() {
    for (field, os, arch, expected) in [
        ("envelope", "darwin", "aarch64", true),
        ("envelope", "windows", "x86_64", true),
        ("windows", "windows", "x86_64", true),
        ("windows", "darwin", "aarch64", false),
    ] {
        let mock = MockServer::start_with(move |_,base| Some(Reply::json(json!({"envelope":vectors()[field],"downloadUrl":format!("{base}/object"),"expiresAt":now()+299})))).unwrap();
        let client = RuntimeArtifactClient::new(&mock.base, public_key()).unwrap();
        let pointer = vectors()[field]["manifest"]["pointerKey"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            client
                .resolve(
                    "genright",
                    &pointer,
                    "test-license",
                    &RuntimeArtifactTarget::new(os, arch)
                )
                .is_ok(),
            expected
        );
    }
}

#[test]
fn signed_size_pin_and_cross_client_trust_are_enforced() {
    let mock = MockServer::start_with(move |req,base| {
        if req.method == "POST" { Some(Reply::json(json!({"envelope":vectors()["wrongSize"],"downloadUrl":format!("{base}/object"),"expiresAt":now()+299}))) }
        else { Some(Reply::bytes("application/octet-stream",body()).with_header("ETag","\"fixed\"")) }
    }).unwrap();
    let client = RuntimeArtifactClient::new(&mock.base, public_key()).unwrap();
    let resolved = client
        .resolve("genright", &pointer(), "test-license", &target())
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path(), "genright"));
    assert!(matches!(
        client.install(
            &resolved,
            &store,
            "size",
            &InstallOptions::default(),
            |_| {}
        ),
        Err(RuntimeArtifactError::SizeMismatch)
    ));
    assert!(store.active_dir("size").is_none());
    let foreign = RuntimeArtifactClient::new(
        &mock.base,
        [
            115, 7, 50, 136, 83, 85, 196, 64, 12, 17, 85, 146, 164, 44, 145, 184, 176, 39, 125, 70,
            12, 178, 181, 213, 156, 30, 40, 21, 167, 77, 93, 34,
        ],
    )
    .unwrap();
    assert!(matches!(
        foreign.install(
            &resolved,
            &store,
            "foreign",
            &InstallOptions::default(),
            |_| {}
        ),
        Err(RuntimeArtifactError::SignatureMismatch)
    ));
}

/// Test stores may reach the local mock server on 127.0.0.1 and nothing else.
fn local(store: Store) -> Store {
    store
        .with_http_options(rightkit_models::http::ModelHttpOptions {
            proxy: rightkit_models::http::ProxyMode::None,
            ..Default::default()
        })
        .with_address_policy(
            rightkit_http::AddressPolicy::public_web()
                .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        )
}
