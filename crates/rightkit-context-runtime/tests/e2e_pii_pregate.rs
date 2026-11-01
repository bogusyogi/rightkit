//! A4: production Cortex proposal/push boundaries over isolated SQLite stores.
//! Environment policy scenarios are serialized, and an explicit hash embedder keeps DLP acceptance independent of model setup.
use rightkit_context_runtime::{cortex_lifecycle, MemDb, MemoryStore};
use rightkit_db::rusqlite::types::ValueRef;
use rightkit_memory_core::HashEmbedder;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Environment {
    fn set(values: &[(&'static str, Option<String>)]) -> Self {
        let mut previous = Vec::new();
        for (key, value) in values {
            previous.push((*key, std::env::var_os(key)));
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        Self(previous)
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        for (key, previous) in self.0.drain(..).rev() {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct Fixture {
    store: MemoryStore,
    temp: tempfile::TempDir,
    _environment: Environment,
    _lock: MutexGuard<'static, ()>,
}
impl Fixture {
    fn new(policy: Option<&str>, scopes: Option<&str>) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let environment = Environment::set(&[
            (
                "RIGHTKIT_CONTEXT_WORKSPACE_ROOT",
                Some(temp.path().to_string_lossy().into_owned()),
            ),
            (
                "RIGHTKIT_CONTEXT_EVENT_DB",
                Some(
                    temp.path()
                        .join("events.sqlite3")
                        .to_string_lossy()
                        .into_owned(),
                ),
            ),
            (
                "RIGHTKIT_CONTEXT_CORTEX_PII_POLICY",
                policy.map(str::to_owned),
            ),
            (
                "RIGHTKIT_CONTEXT_CORTEX_PII_SCOPES",
                scopes.map(str::to_owned),
            ),
        ]);
        let store = MemoryStore::try_open_with_embedder(
            MemDb::open(temp.path().join("cortex.sqlite3")).unwrap(),
            Arc::new(HashEmbedder::new()),
        )
        .unwrap();
        Self {
            store,
            temp,
            _environment: environment,
            _lock: lock,
        }
    }

    fn propose(&self, scope: &str, text: &str) -> Result<Value, cortex_lifecycle::LifecycleError> {
        cortex_lifecycle::propose(
            &self.store,
            "repo-pii",
            scope,
            &json!({
                "text":text,"producer":"agent","epistemicClass":"reported",
            }),
        )
    }

    fn push(
        &self,
        scope: &str,
        request: &str,
        body: &str,
    ) -> Result<Value, cortex_lifecycle::LifecycleError> {
        cortex_lifecycle::agent_memory_push(
            &self.store,
            "repo-pii",
            scope,
            request,
            "caller-pii",
            body,
        )
    }

    fn source(&self, scope: &str, request: &str) -> (Vec<u8>, String) {
        self.store.db().lock().query_row(
            "SELECT raw_body,caller_input_json FROM cortex_agent_memory_source_v1 WHERE scope_id=?1 AND request_id=?2",
            [scope, request], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap()
    }

    fn assert_not_persisted(&self, forbidden: &[&str]) {
        for db in [self.store.db().lock(), self.store.db().lock_events()] {
            let tables = db
                .prepare("SELECT name FROM sqlite_master WHERE type='table'")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            for table in tables {
                let mut statement = db
                    .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
                    .unwrap();
                let columns = statement.column_count();
                let mut rows = statement.query([]).unwrap();
                while let Some(row) = rows.next().unwrap() {
                    for column in 0..columns {
                        let bytes = match row.get_ref(column).unwrap() {
                            ValueRef::Text(bytes) | ValueRef::Blob(bytes) => bytes,
                            _ => continue,
                        };
                        for text in forbidden {
                            assert!(
                                !bytes
                                    .windows(text.len())
                                    .any(|window| window == text.as_bytes()),
                                "PII persisted in {table}"
                            );
                        }
                    }
                }
            }
        }
        // Include WAL/SHM and database files: pre-policy text must never have
        // been written and subsequently removed from the live projection.
        for entry in std::fs::read_dir(self.temp.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                let bytes = std::fs::read(path).unwrap();
                for text in forbidden {
                    assert!(
                        !bytes
                            .windows(text.len())
                            .any(|window| window == text.as_bytes()),
                        "PII present in persisted file"
                    );
                }
            }
        }
    }
}

#[test]
fn each_pii_class_has_positive_and_negative_production_cases() {
    let fixture = Fixture::new(Some("deny"), None);
    let cases = [
        (
            "email",
            "Contact alice@example.com",
            "alice at example dot com",
        ),
        ("phone", "Call +12025550123", "Call 123-4567"),
        ("phone", "Call (202) 555-0123", "Call 12-34"),
        (
            "payment_card",
            "Card 4111 1111 1111 1111",
            "Card 4111 1111 1111 1112",
        ),
        ("government_id", "SSN 123-45-6789", "SSN 666-45-6789"),
        ("government_id", "NINO AB 12 34 56 C", "NINO BG123456A"),
        (
            "government_id",
            "IBAN GB82 WEST 1234 5698 7654 32",
            "IBAN GB83 WEST 1234 5698 7654 32",
        ),
    ];
    for (class, positive, negative) in cases {
        let error = fixture.propose("pii", positive).unwrap_err();
        assert_eq!(error.code, "dlp_denied");
        assert!(error.message.contains(&format!("\"class\":\"{class}\"")));
        assert!(error.message.contains("\"count\":1"));
        assert!(!error.message.contains(positive));
        fixture.propose("pii", negative).unwrap();
        assert_eq!(
            fixture.push("pii", positive, positive).unwrap_err().code,
            "dlp_denied"
        );
    }
}

#[test]
fn invalid_luhn_versions_dates_and_hashes_are_not_pii() {
    let fixture = Fixture::new(Some("deny"), None);
    for text in [
        "Card 4111-1111-1111-1112",
        "Version v1.2.3",
        "Version 1.234.567.890",
        "Date 2026-10-08 12:30:45",
        "hash a2025550123abcdef",
        "hash 12345678901234567890123456789012",
    ] {
        let receipt = fixture.propose("pii", text).unwrap();
        assert_eq!(receipt["pregate"]["pii"]["classes"], json!([]));
    }
}

#[test]
fn off_is_default_and_preserves_submitted_bytes() {
    let fixture = Fixture::new(None, None);
    let body = "Contact alice@example.com, call +12025550123.";
    let receipt = fixture.push("pii", "off-1", body).unwrap();
    assert_eq!(receipt["pregate"]["pii"]["policy"], "off");
    assert_eq!(receipt["pregate"]["pii"]["redacted"], false);
    assert_eq!(fixture.source("pii", "off-1").0, body.as_bytes());
    let content: String = fixture
        .store
        .db()
        .lock()
        .query_row(
            "SELECT content FROM memories WHERE id=?1",
            [receipt["memoryId"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(content, body);
    fixture.propose("pii", body).unwrap();
}

#[test]
fn redact_rewrites_canonical_body_ctx043_source_and_optional_metadata() {
    let fixture = Fixture::new(Some("redact"), None);
    let body =
        "Contact alice@example.com; call +12025550123; card 4111-1111-1111-1111; SSN 123-45-6789.";
    let expected = "Contact [REDACTED:email]; call [REDACTED:phone]; card [REDACTED:payment_card]; SSN [REDACTED:government_id].";
    let input = cortex_lifecycle::AgentPushInputV1 {
        keywords: vec!["alice@example.com".into()],
        caller_input_json: Some(
            json!({"bodyCopy":body,"keywords":["alice@example.com"]}).to_string(),
        ),
        ..Default::default()
    };
    let receipt = cortex_lifecycle::agent_memory_push_with_input(
        &fixture.store,
        "repo-pii",
        "pii",
        "redact-1",
        "caller-pii",
        body,
        &input,
    )
    .unwrap();
    assert_eq!(receipt["pregate"]["pii"]["redacted"], true);
    let (source, optional) = fixture.source("pii", "redact-1");
    assert_eq!(source, expected.as_bytes());
    assert!(optional.contains("[REDACTED:email]"));
    let content: String = fixture
        .store
        .db()
        .lock()
        .query_row(
            "SELECT content FROM memories WHERE id=?1",
            [receipt["memoryId"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(content, expected);
    let replay = cortex_lifecycle::agent_memory_push_with_input(
        &fixture.store,
        "repo-pii",
        "pii",
        "redact-1",
        "caller-pii",
        body,
        &input,
    )
    .unwrap();
    assert_eq!(replay["status"], "replayed");
    assert_eq!(replay["pregate"]["pii"], receipt["pregate"]["pii"]);
    let forbidden = [
        "alice@example.com",
        "+12025550123",
        "4111-1111-1111-1111",
        "123-45-6789",
    ];
    for text in forbidden {
        assert!(!receipt.to_string().contains(text));
    }
    fixture.assert_not_persisted(&forbidden);
}

#[test]
fn nested_proposal_redaction_has_durable_unforgeable_count_receipt() {
    let fixture = Fixture::new(Some("redact"), None);
    let receipt = cortex_lifecycle::propose(
        &fixture.store,
        "repo-pii",
        "pii",
        &json!({
            "text":"Contact alice@example.com & alice@example.com",
            "producer":"agent","epistemicClass":"reported",
            "payload":{"body":"Card 4111 1111 1111 1111"},
            "_cortexPiiPregate":{"policy":"off","redacted":false,"classes":[]},
        }),
    )
    .unwrap();
    assert_eq!(receipt["pregate"]["pii"]["redacted"], true);
    assert_eq!(
        receipt["pregate"]["pii"]["classes"],
        json!([
            {"class":"email","count":2},{"class":"payment_card","count":1},
        ])
    );
    let status = cortex_lifecycle::proposal_status(
        &fixture.store,
        "repo-pii",
        "pii",
        receipt["proposalId"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(status["pregate"]["pii"], receipt["pregate"]["pii"]);
    fixture.assert_not_persisted(&["alice@example.com", "4111 1111 1111 1111"]);
}

#[test]
fn pii_receipts_preserve_strict_multiwriter_emission_validation() {
    use rightkit_adapt_taste::authority::PrecedenceTier;
    use rightkit_adapt_taste::multiwriter::{validate_proposal_emission, WriterRequestV1};

    for policy in ["deny", "redact"] {
        let fixture = Fixture::new(Some(policy), None);
        let writer = if policy == "redact" {
            "alice@example.com"
        } else {
            "writer-pii"
        };
        let requests = [WriterRequestV1::new(
            "repo-pii",
            "pii",
            "record-pii",
            "v1",
            writer,
            1,
            "payload-pii",
            PrecedenceTier::CurrentExplicitUserInstruction,
        )];
        let receipt =
            cortex_lifecycle::propose_multiwriter(&fixture.store, "repo-pii", "pii", &requests)
                .unwrap();
        let emission: String = fixture
            .store
            .db()
            .lock_events()
            .query_row(
                "SELECT emission_json FROM membrane_knowledge_proposal WHERE proposal_id=?1",
                [receipt["proposalId"].as_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        validate_proposal_emission("repo-pii", "pii", &serde_json::from_str(&emission).unwrap())
            .unwrap();
        if policy == "redact" {
            assert!(!receipt.to_string().contains(writer));
            fixture.assert_not_persisted(&[writer]);
        }
    }
}

#[test]
fn deny_leaves_no_candidate_bytes_and_reports_class_count_only() {
    let fixture = Fixture::new(Some("deny"), None);
    let body = "alice@example.com, alice@example.com";
    for error in [
        fixture.push("pii", "deny-1", body).unwrap_err(),
        fixture.propose("pii", body).unwrap_err(),
    ] {
        assert_eq!(error.code, "dlp_denied");
        assert!(error.message.contains("\"class\":\"email\""));
        assert!(error.message.contains("\"count\":2"));
        assert!(!error.message.contains("alice@example.com"));
    }
    fixture.assert_not_persisted(&["alice@example.com"]);
}

#[test]
fn host_scope_overrides_global_policy_and_invalid_config_fails_closed() {
    let fixture = Fixture::new(
        Some("deny"),
        Some(r#"{"redacted":"redact","exempt":"off"}"#),
    );
    let body = "Contact alice@example.com";
    assert_eq!(
        fixture.propose("redacted", body).unwrap()["pregate"]["pii"]["redacted"],
        true
    );
    assert_eq!(
        fixture.propose("exempt", body).unwrap()["pregate"]["pii"]["policy"],
        "off"
    );
    assert_eq!(
        fixture.propose("other", body).unwrap_err().code,
        "dlp_denied"
    );
    std::env::set_var("RIGHTKIT_CONTEXT_CORTEX_PII_SCOPES", r#"{"other":"bogus"}"#);
    assert_eq!(
        fixture.propose("other", "ordinary text").unwrap_err().code,
        "dlp_policy_invalid"
    );
}

#[test]
fn secret_material_denies_under_every_pii_policy() {
    for policy in ["off", "redact", "deny"] {
        let fixture = Fixture::new(Some(policy), None);
        let secret = concat!("gh", "p_abcdefghijklmnopqrstuvwxyz");
        let body = format!("alice@example.com {secret}");
        for error in [
            fixture.push("pii", "secret-1", &body).unwrap_err(),
            fixture.propose("pii", &body).unwrap_err(),
        ] {
            assert_eq!(error.code, "dlp_denied");
            assert!(error.message.contains("github_token"));
            assert!(!error.message.contains(secret));
            assert!(!error.message.contains("alice@example.com"));
        }
        fixture.assert_not_persisted(&[secret, "alice@example.com"]);
    }
}
