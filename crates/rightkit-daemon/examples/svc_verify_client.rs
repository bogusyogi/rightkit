//! Client for the host-verification tests: `svc_verify_client <mode> <service> [<expected-exe>]`.
//! Prints one line and exits 0. The tests set the environment of this process to play a launcher
//! that redirects it.
//!
//! * `resolve`: `ClientConfig::resolve_suite()` with the environment honoured: `ROOT <path>`.
//! * `resolve-hardened`: the same with `ignore_env_overrides(true)`.
//! * `resolve-env-suite-hardened`: a `Suite::resolve()` suite handed to a hardened configuration,
//!   which must refuse it: `ERR suite ...`.
//! * `connect`: `Suite::resolve()` then `Client::connect_with` (the host is not authenticated).
//! * `expect`: environment honoured, `Client::connect_expecting` for `<expected-exe>`.
//! * `hardened`: `ignore_env_overrides(true)` and `Client::connect_expecting`.
//!
//! Connecting modes print `OK pid=<verified host pid, or none>`, `ERR service <code>`, or
//! `ERR host_identity <detail>`.
use rightkit_daemon::*;
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 2 {
        eprintln!("usage: svc_verify_client <mode> <service> [<expected-exe>]");
        std::process::exit(2);
    }
    let (mode, service) = (a[0].as_str(), a[1].as_str());
    let deadline = Some(Duration::from_secs(5));
    let config = || ClientConfig::new(service, "alpha").deadline(deadline);
    let expected = || HostExpectation::executable(a.get(2).cloned().unwrap_or_default());
    match mode {
        "resolve" => root(config().resolve_suite()),
        "resolve-hardened" => root(config().ignore_env_overrides(true).resolve_suite()),
        "resolve-env-suite-hardened" => match Suite::resolve() {
            Ok(suite) => root(
                config()
                    .suite(suite)
                    .ignore_env_overrides(true)
                    .resolve_suite(),
            ),
            Err(e) => println!("ERR suite {e}"),
        },
        "connect" => match Suite::resolve() {
            Ok(suite) => outcome(
                Client::connect_with(&suite, service, "alpha", deadline)
                    .map_err(ConnectError::from),
            ),
            Err(e) => println!("ERR suite {e}"),
        },
        "expect" => outcome(Client::connect_expecting(&config(), &expected())),
        "hardened" => outcome(Client::connect_expecting(
            &config().ignore_env_overrides(true),
            &expected(),
        )),
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}

fn root(resolved: Result<Suite, SuiteError>) {
    match resolved {
        Ok(suite) => println!("ROOT {}", suite.root().display()),
        Err(e) => println!("ERR suite {e}"),
    }
}

fn outcome(connected: Result<Client, ConnectError>) {
    match connected {
        Ok(client) => println!(
            "OK pid={}",
            client
                .verified_host()
                .map(|host| host.pid.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        Err(ConnectError::Service(e)) => println!("ERR service {}", e.code),
        Err(ConnectError::HostIdentity(e)) => println!("ERR host_identity {e}"),
        Err(e) => println!("ERR other {e}"),
    }
}
