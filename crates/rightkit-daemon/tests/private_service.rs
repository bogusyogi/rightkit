//! The private per-run child service pattern (issue #269): a host that requires a secret on every
//! frame, and clients that do or do not hold it.

use rightkit_daemon::*;
use serde_json::{json, Value};
use std::sync::Arc;

struct Echo;

impl Handler for Echo {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            "echo" => Ok(params.clone()),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

fn start(secret: &PrivateSecret) -> (tempfile::TempDir, Suite, Hosted) {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let config = ServiceConfig::new(
        "private-svc",
        "engine",
        1,
        "1.0",
        Allowlist::new().allow("app", ["echo"]),
    )
    .private_secret(secret.clone());
    let hosted = host(&suite, config, Arc::new(Echo)).expect("host starts");
    (dir, suite, hosted)
}

#[test]
fn a_client_with_the_secret_is_served() {
    let secret = PrivateSecret::generate().unwrap();
    let (_dir, suite, _hosted) = start(&secret);
    let config = ClientConfig::new("private-svc", "app")
        .suite(suite)
        .secret(secret);
    let client = Client::connect_config(&config).expect("connects");
    assert_eq!(client.call("echo", json!({"n": 1})).unwrap(), json!({"n": 1}));
    // Every later request carries the secret too.
    assert_eq!(client.call("echo", json!({"n": 2})).unwrap(), json!({"n": 2}));
}

#[test]
fn a_client_without_the_secret_is_refused_at_hello() {
    let secret = PrivateSecret::generate().unwrap();
    let (_dir, suite, _hosted) = start(&secret);
    let error = match Client::connect(&suite, "private-svc", "app") {
        Ok(_) => panic!("a client without the secret must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.code, ERROR_UNAUTHENTICATED);
    assert!(!error.retryable);
}

#[test]
fn a_client_with_a_wrong_secret_is_refused_at_hello() {
    let secret = PrivateSecret::generate().unwrap();
    let (_dir, suite, _hosted) = start(&secret);
    let other = PrivateSecret::generate().unwrap();
    let config = ClientConfig::new("private-svc", "app")
        .suite(suite)
        .secret(other);
    let error = match Client::connect_config(&config) {
        Ok(_) => panic!("a wrong secret must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.code, ERROR_UNAUTHENTICATED);
}

#[test]
fn a_host_with_no_secret_ignores_the_field() {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let config = ServiceConfig::new(
        "plain-svc",
        "engine",
        1,
        "1.0",
        Allowlist::new().allow("app", ["echo"]),
    );
    let _hosted = host(&suite, config, Arc::new(Echo)).unwrap();
    let client = Client::connect(&suite, "plain-svc", "app").unwrap();
    assert_eq!(client.call("echo", json!(7)).unwrap(), json!(7));
}
