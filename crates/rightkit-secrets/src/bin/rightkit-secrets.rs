//! Diagnostic CLI. It can report presence, store/delete a secret read from stdin, and ensure a
//! random key exists. It has NO command that prints a secret.
//!
//!   rightkit-secrets <status|set|delete|ensure-key> --namespace N --account A [--replace]
//!       [--isolated-keychain PATH --keychain-password PW]   (macOS test keychain under the temp directory; never the login keychain; needs the `isolated-test-keychain` feature)
//!
//! `set` creates the credential and refuses (`"stored":false`) when one exists; `--replace` is
//! the explicit instruction to overwrite it.

use rightkit_secrets::{Config, Namespace, Secret, Store, WriteIntent};
use std::io::Read;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(e) => {
            println!(
                "{}",
                serde_json::json!({ "ok": false, "error": e.to_string() })
            );
            1
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> rightkit_secrets::Result<serde_json::Value> {
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let missing = |what: &'static str| rightkit_secrets::Error::Invalid {
        what,
        reason: "required".into(),
    };
    let ns = Namespace::new(arg(args, "--namespace").ok_or(missing("--namespace"))?)?;
    let account = arg(args, "--account").ok_or(missing("--account"))?;
    #[allow(unused_mut)]
    let mut cfg = Config::new(ns);
    if let Some(path) = arg(args, "--isolated-keychain") {
        #[cfg(all(target_os = "macos", feature = "isolated-test-keychain"))]
        {
            let pw = arg(args, "--keychain-password");
            let kc = rightkit_secrets::macos::KeychainBackend::open_isolated_file(
                std::path::Path::new(&path),
                pw.as_deref(),
            )?;
            cfg = cfg.backend(std::sync::Arc::new(kc));
        }
        #[cfg(not(all(target_os = "macos", feature = "isolated-test-keychain")))]
        {
            let _ = path;
            return Err(rightkit_secrets::Error::Unavailable(
                "--isolated-keychain needs macOS and the isolated-test-keychain feature".into(),
            ));
        }
    }
    let store = Store::open(cfg);
    let diag = store.diagnostics();
    match cmd {
        "status" => Ok(
            serde_json::json!({ "ok": true, "account": account, "present": store.exists(&account)?, "store": diag }),
        ),
        "set" => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| rightkit_secrets::Error::Io(e.to_string()))?;
            let intent = if args.iter().any(|a| a == "--replace") {
                WriteIntent::Replace
            } else {
                WriteIntent::CreateOnly
            };
            let stored = store.put(
                &account,
                &Secret::from_string(s.trim_end_matches(['\n', '\r'])),
                intent,
            )?;
            Ok(serde_json::json!({ "ok": true, "stored": stored }))
        }
        "delete" => Ok(serde_json::json!({ "ok": true, "deleted": store.delete(&account)? })),
        "ensure-key" => {
            let (key, created) = store.get_or_create_key(&account, 32)?;
            Ok(serde_json::json!({ "ok": true, "created": created, "length": key.len() }))
        }
        _ => Err(rightkit_secrets::Error::Invalid {
            what: "command",
            reason: "use status|set|delete|ensure-key".into(),
        }),
    }
}
