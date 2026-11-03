//! Diagnostic CLI. It can report presence, store/delete a secret read from stdin, and ensure a
//! random key exists, or pass one credential directly to a native child's environment.
//! The CLI never prints secret bytes; an executed child controls its own output.
//!
//!   rightkit-credentials <status|set|delete|ensure-key> --namespace N --account A [--replace]
//!   rightkit-credentials exec --namespace N --account A --env NAME -- /absolute/native/executable [args...]
//! `exec` requires an unguarded durable OS store, inherits child output, and returns its exit
//! status. The selected native executable receives the secret only in its own environment.
//!
//! `set` creates the credential and refuses (`"stored":false`) when one exists; `--replace` is
//! the explicit instruction to overwrite it.

use rightkit_credentials::{Config, Namespace, Secret, Store, WriteIntent};
use std::io::Read;
use std::process::Command;

fn invalid(what: &'static str, reason: &str) -> rightkit_credentials::Error {
    rightkit_credentials::Error::Invalid {
        what,
        reason: reason.into(),
    }
}

fn exec_command(
    store: &Store,
    account: &str,
    env: &str,
    executable: &str,
    args: &[String],
) -> rightkit_credentials::Result<Command> {
    if env.is_empty() || env.contains(['=', '\0']) {
        return Err(invalid(
            "--env",
            "requires a nonempty environment name without '=' or NUL",
        ));
    }
    let path = std::path::Path::new(executable);
    if !path.is_absolute() {
        return Err(invalid(
            "executable",
            "requires an absolute native executable path",
        ));
    }
    if cfg!(windows)
        && !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return Err(invalid(
            "executable",
            "Windows exec requires an .exe binary",
        ));
    }
    // Scripts (including Windows batch files) must never acquire an implicit shell here.
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map_err(|_| invalid("executable", "cannot read native executable header"))?;
    let native = if cfg!(windows) {
        magic.starts_with(b"MZ")
    } else if cfg!(target_os = "macos") {
        matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
    } else {
        magic == *b"\x7fELF"
    };
    if !native {
        return Err(invalid(
            "executable",
            "requires a native binary, not a script",
        ));
    }
    let store = store.pinned()?;
    if !store.is_durable() || store.guard_active() {
        return Err(invalid(
            "store",
            "exec requires an unguarded durable OS credential store",
        ));
    }
    let secret = store
        .get(account)?
        .ok_or_else(|| invalid("credential", "not found"))?;
    let value = secret.expose_str()?;
    if value.is_empty() || value.contains('\0') {
        return Err(invalid("credential", "requires nonempty UTF-8 without NUL"));
    }
    let mut command = Command::new(path);
    command.args(args).env(env, value);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    Ok(command)
}

fn exec(args: &[String]) -> rightkit_credentials::Result<i32> {
    let split = args
        .iter()
        .position(|s| s == "--")
        .ok_or_else(|| invalid("--", "required before native executable"))?;
    let options = &args[1..split];
    let mut ns = None;
    let mut account = None;
    let mut env = None;
    let (pairs, remainder) = options.as_chunks::<2>();
    for pair in pairs {
        let slot = match pair[0].as_str() {
            "--namespace" => &mut ns,
            "--account" => &mut account,
            "--env" => &mut env,
            _ => {
                return Err(invalid(
                    "option",
                    "exec accepts only --namespace, --account and --env",
                ))
            }
        };
        if slot.replace(pair[1].clone()).is_some() {
            return Err(invalid("option", "duplicate exec option"));
        }
    }
    if !remainder.is_empty() {
        return Err(invalid("option", "missing exec option value"));
    }
    let required = |value: Option<String>, name| value.ok_or_else(|| invalid(name, "required"));
    let ns = Namespace::new(required(ns, "--namespace")?)?;
    let account = required(account, "--account")?;
    let env = required(env, "--env")?;
    let executable = args
        .get(split + 1)
        .ok_or_else(|| invalid("executable", "required"))?;
    let store = Store::open(Config::new(ns));
    let mut command = exec_command(&store, &account, &env, executable, &args[split + 2..])?;
    let result = command
        .status()
        .map_err(|_| invalid("child", "native executable could not run"));
    // Drop Command's environment copy immediately after child completion, before diagnostics.
    drop(command);
    let status = result?;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
    }
    #[cfg(not(unix))]
    {
        Ok(status.code().unwrap_or(1))
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "exec") {
        let code = match exec(&args) {
            Ok(code) => code,
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({ "ok": false, "error": error.to_string() })
                );
                1
            }
        };
        std::process::exit(code);
    }
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

fn run(args: &[String]) -> rightkit_credentials::Result<serde_json::Value> {
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let missing = |what: &'static str| rightkit_credentials::Error::Invalid {
        what,
        reason: "required".into(),
    };
    let ns = Namespace::new(arg(args, "--namespace").ok_or(missing("--namespace"))?)?;
    let account = arg(args, "--account").ok_or(missing("--account"))?;
    let cfg = Config::new(ns);
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
                .map_err(|e| rightkit_credentials::Error::Io(e.to_string()))?;
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
        _ => Err(rightkit_credentials::Error::Invalid {
            what: "command",
            reason: "use status|set|delete|ensure-key|exec".into(),
        }),
    }
}
