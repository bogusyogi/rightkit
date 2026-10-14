//! `rightkit-qa-driver` command-line dispatch. Every effectful subcommand is
//! admitted through one [`EffectGate`] (EFF-001); read-only subcommands
//! (`trusted`, `frontmost`, `windows`, `ax`, `find`) are not effects.
//!
//! Global options (before the subcommand):
//! - `--policy <file>` (or `RIGHTKIT_CONTROL_POLICY=<file>`): admission policy
//!   (see [`Policy`]). Without one the gate is allow-all, so behaviour is
//!   unchanged, but every effect is still validated and settled. A policy
//!   that cannot be read or parsed is fatal (exit 2); it never falls back to
//!   allow-all.
//! - `--events` (or `RIGHTKIT_CONTROL_EVENTS=stderr`): print content-free
//!   lifecycle events to stderr.
//!
//! Exit codes: 0 ok, 1 failed, 2 usage/configuration, 3 denied by admission,
//! 4 cancelled.
use std::path::PathBuf;
use std::sync::Arc;

use crate::admission::{
    AdmissionDecision, AdmissionHook, Effect, EffectGate, EffectKind, EffectRequest, SettleOutcome,
};
use crate::events::{ControlEvent, EventSink};

/// One policy rule: `allow|deny <kind|*> [method|*]`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    allow: bool,
    kind: Option<EffectKind>,
    method: Option<String>,
}

/// A file-configured admission hook. Line format (`#` starts a comment):
///
/// ```text
/// default deny            # decision when no rule matches (required)
/// allow capture           # any capture
/// allow input click       # only click among input effects
/// deny * *                # explicit catch-all
/// ```
///
/// Kinds: `navigate input action script capture process command` or `*`.
/// The first matching rule wins; otherwise `default` decides. A policy with
/// no `default` line is rejected, so there is no implicit allow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    rules: Vec<Rule>,
    default_allow: bool,
}

fn parse_kind(s: &str) -> Option<Option<EffectKind>> {
    Some(Some(match s {
        "*" => return Some(None),
        "navigate" => EffectKind::Navigate,
        "input" => EffectKind::Input,
        "action" => EffectKind::Action,
        "script" => EffectKind::Script,
        "capture" => EffectKind::Capture,
        "process" => EffectKind::Process,
        "command" => EffectKind::Command,
        _ => return None,
    }))
}

fn parse_decision(s: &str) -> Option<bool> {
    match s {
        "allow" => Some(true),
        "deny" => Some(false),
        _ => None,
    }
}

impl Policy {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut rules = Vec::new();
        let mut default_allow = None;
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let at = |m: &str| format!("policy line {}: {m}", n + 1);
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                ["default", d] => {
                    if default_allow.is_some() {
                        return Err(at("duplicate default"));
                    }
                    default_allow =
                        Some(parse_decision(d).ok_or_else(|| at("default must be allow or deny"))?);
                }
                [d, kind, rest @ ..] if rest.len() <= 1 => {
                    let allow = parse_decision(d)
                        .ok_or_else(|| at("rule must start with allow or deny"))?;
                    let kind = parse_kind(kind)
                        .ok_or_else(|| at(&format!("unknown effect kind {kind:?}")))?;
                    let method = rest.first().filter(|m| **m != "*").map(|m| m.to_string());
                    rules.push(Rule {
                        allow,
                        kind,
                        method,
                    });
                }
                _ => {
                    return Err(at(
                        "expected `default allow|deny` or `allow|deny <kind|*> [method|*]`",
                    ))
                }
            }
        }
        let default_allow = default_allow.ok_or("policy has no `default allow|deny` line")?;
        Ok(Self {
            rules,
            default_allow,
        })
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("policy {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("policy {}: {e}", path.display()))
    }
}

impl AdmissionHook for Policy {
    fn approve(&self, r: &EffectRequest) -> AdmissionDecision {
        let hit = self.rules.iter().find(|rule| {
            rule.kind.is_none_or(|k| k == r.kind)
                && rule.method.as_deref().is_none_or(|m| m == r.method)
        });
        let allow = hit.map_or(self.default_allow, |rule| rule.allow);
        if allow {
            AdmissionDecision::Allow
        } else {
            AdmissionDecision::deny(format!("policy denies {} {}", r.kind.label(), r.method))
        }
    }
}

/// Global CLI configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliConfig {
    pub policy: Option<PathBuf>,
    pub events: bool,
}

/// Split leading global options from the subcommand arguments. `env` is
/// consulted for `RIGHTKIT_CONTROL_POLICY` / `RIGHTKIT_CONTROL_EVENTS`; flags
/// win over the environment.
pub fn parse_global(
    args: &[String],
    env: impl Fn(&str) -> Option<String>,
) -> Result<(CliConfig, Vec<String>), String> {
    let mut cfg = CliConfig {
        policy: env("RIGHTKIT_CONTROL_POLICY")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from),
        events: env("RIGHTKIT_CONTROL_EVENTS").is_some_and(|v| v == "stderr" || v == "1"),
    };
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--policy" => {
                let p = args.get(i + 1).ok_or("--policy needs a file")?;
                cfg.policy = Some(PathBuf::from(p));
                i += 2;
            }
            "--events" => {
                cfg.events = true;
                i += 1;
            }
            _ => break,
        }
    }
    Ok((cfg, args[i..].to_vec()))
}

fn stderr_sink() -> EventSink {
    Arc::new(|e: &ControlEvent| eprintln!("rightkit-qa-driver event: {e:?}"))
}

/// Build the CLI gate: allow-all unless a policy is configured.
pub fn gate_for(cfg: &CliConfig) -> Result<Arc<EffectGate>, String> {
    let mut gate = match &cfg.policy {
        Some(p) => EffectGate::new(Policy::load(p)?),
        None => EffectGate::allow_all(),
    };
    if cfg.events {
        gate = gate.with_events(stderr_sink());
    }
    Ok(Arc::new(gate))
}

/// Exit code for a settled effect; prints the refusal/failure to stderr.
pub fn exit_code<T>(effect: &Effect<T>) -> i32 {
    let st = &effect.settlement;
    let reason = st.reason.clone().unwrap_or_default();
    match st.outcome {
        SettleOutcome::Ok => 0,
        SettleOutcome::Denied => {
            eprintln!("denied: {reason}");
            3
        }
        SettleOutcome::Failed => {
            eprintln!("{reason}");
            1
        }
        SettleOutcome::Cancelled => {
            eprintln!("cancelled: {reason}");
            4
        }
    }
}

pub const USAGE: &str = "usage: rightkit-qa-driver [--policy FILE] [--events] trusted|frontmost|launch|windows|ax|find|click|click-text|press|keywin|keyraw|hover|drag|scroll|type|key|shot|serve <pid> ...";

/// Run one subcommand (`args` excludes global options). Returns the process
/// exit code. Effects go through `gate`; nothing effectful bypasses it.
#[cfg(target_os = "macos")]
pub fn run(args: &[String], gate: Arc<EffectGate>) -> i32 {
    use crate::keys;
    use crate::mac::{self, Gated};

    let g = Gated::new(gate.clone());
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let num = |i: usize| arg(i).parse::<f64>().unwrap_or(0.0);
    let pid = arg(1).parse::<i32>().unwrap_or(0);
    let target = format!("pid:{pid}");
    let window = |p: i32| mac::main_window(p).ok_or_else(|| format!("no window for pid {p}"));
    let coords = |from: usize, n: usize| {
        let v: Vec<String> = (from..from + n).map(|i| num(i).to_string()).collect();
        format!("[{}]", v.join(","))
    };
    let not_found = || "not found".to_string();

    match arg(0) {
        "trusted" => println!("{}", mac::accessibility_trusted()),
        "frontmost" => println!(
            "{} {}",
            mac::frontmost_pid().unwrap_or(0),
            mac::frontmost_name().unwrap_or_default()
        ),
        "windows" => {
            for w in mac::windows(pid) {
                println!(
                    "id={} layer={} onscreen={} {:.0},{:.0} {:.0}x{:.0} {:?}",
                    w.id, w.layer, w.onscreen, w.x, w.y, w.w, w.h, w.name
                );
            }
        }
        "ax" => {
            for l in mac::ax_dump(pid, arg(2).parse().unwrap_or(12), 20_000) {
                println!("{l}")
            }
        }
        "find" => match mac::ax_find(pid, arg(2), false) {
            Some(n) => println!("{} {:?} {:?}", n.role, n.text, n.frame),
            None => {
                eprintln!("not found");
                return 1;
            }
        },
        "launch" => {
            // launch <App.app> <exe-name> [K=V ...]
            let env: Vec<(String, String)> = args[3.min(args.len())..]
                .iter()
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.into(), v.into()))
                .collect();
            let e = g.launch_hidden(arg(1), arg(2), &env);
            if let Some(p) = e.value {
                println!("{p}");
            }
            return exit_code(&e);
        }
        // Composite commands admit first, then resolve the window/element
        // inside the executor: a denial does no lookup and posts nothing.
        "click" => {
            let (x, y) = (num(2), num(3));
            let e = g.admit(EffectKind::Input, "click", target, coords(2, 2), || {
                mac::click(pid, window(pid)?.id, x, y);
                Ok(())
            });
            return exit_code(&e);
        }
        "hover" => {
            let (x, y) = (num(2), num(3));
            let e = g.admit(EffectKind::Input, "hover", target, coords(2, 2), || {
                mac::hover(pid, window(pid)?.id, x, y);
                Ok(())
            });
            return exit_code(&e);
        }
        "drag" => {
            let (a, b) = ((num(2), num(3)), (num(4), num(5)));
            let e = g.admit(EffectKind::Input, "drag", target, coords(2, 4), || {
                mac::drag(pid, window(pid)?.id, a, b, 20);
                Ok(())
            });
            return exit_code(&e);
        }
        "scroll" => {
            let (x, y, dy) = (num(2), num(3), num(4) as i32);
            let e = g.admit(EffectKind::Input, "scroll", target, coords(2, 3), || {
                mac::scroll(pid, window(pid)?.id, x, y, dy);
                Ok(())
            });
            return exit_code(&e);
        }
        "click-text" => {
            let needle = arg(2);
            let p = format!("{{\"text\":\"{}\"}}", crate::json::esc(needle));
            let e = g.admit(EffectKind::Input, "click_text", target, p, || {
                let n = mac::ax_find(pid, needle, false).ok_or_else(not_found)?;
                mac::click_node(pid, &n)
            });
            return exit_code(&e);
        }
        "press" => {
            let needle = arg(2);
            let p = format!("{{\"text\":\"{}\"}}", crate::json::esc(needle));
            let e = g.admit(EffectKind::Action, "press", target, p, || {
                let n = mac::ax_find(pid, needle, false).ok_or_else(not_found)?;
                Ok(mac::ax_press(&n))
            });
            if let Some(v) = e.value {
                println!("{v}");
            }
            return exit_code(&e);
        }
        "keywin" => {
            let attrs: Vec<&str> = args
                .get(2..)
                .unwrap_or(&[])
                .iter()
                .map(String::as_str)
                .collect();
            let e = g.ax_make_key(pid, &attrs);
            if let Some(v) = &e.value {
                println!("{v:?}");
            }
            return exit_code(&e);
        }
        "keyraw" => {
            let e = g.admit(
                EffectKind::Action,
                "key_without_raise",
                target,
                String::new(),
                || Ok(mac::key_without_raise(pid, window(pid)?.id)),
            );
            if let Some(v) = e.value {
                println!("{v}");
            }
            return exit_code(&e);
        }
        "type" => return exit_code(&g.type_text(pid, arg(2))),
        "key" => match keys::chord(arg(2)) {
            Some((k, f)) => return exit_code(&g.key(pid, k, f)),
            None => {
                eprintln!("unknown key {}", arg(2));
                return 1;
            }
        },
        "shot" => {
            let out = arg(2);
            let p = format!("{{\"out\":\"{}\"}}", crate::json::esc(out));
            let e = g.admit(EffectKind::Capture, "capture", target, p, || {
                mac::capture_window(window(pid)?.id, out)
                    .then_some(())
                    .ok_or_else(|| "screencapture failed".to_string())
            });
            return exit_code(&e);
        }
        "serve" => return serve(args, pid, gate),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    }
    0
}

/// `serve <pid> <port|0> <credential-file>`: the WebDriver bridge, with its
/// click / send keys / screenshot admitted through `gate`.
#[cfg(target_os = "macos")]
fn serve(args: &[String], pid: i32, gate: Arc<EffectGate>) -> i32 {
    use crate::webdriver;
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    // The per-launch credential is generated here, written owner-only to the
    // file (never printed), and required as `Authorization: Bearer` on every request.
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let port: u16 = arg(2).parse().unwrap_or(0);
    let cred_path = arg(3);
    if cred_path.is_empty() {
        eprintln!("usage: rightkit-qa-driver serve <pid> <port|0> <credential-file>");
        return 2;
    }
    let mut raw = [0u8; 32];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut raw))
        .is_err()
    {
        eprintln!("no system randomness");
        return 1;
    }
    let cred: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let wrote = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(cred_path)
        .and_then(|mut f| f.write_all(cred.as_bytes()));
    if let Err(e) = wrote {
        eprintln!("credential file {cred_path}: {e}");
        return 1;
    }
    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let mut s = webdriver::Server::new(webdriver::macos::AxBackend::new(pid)).with_gate(gate);
    eprintln!(
        "rightkit-qa-driver WebDriver bridge on 127.0.0.1:{bound} -> pid {pid} (bearer credential in {cred_path})"
    );
    if let Err(e) = s.serve(listener, &cred) {
        eprintln!("{e}");
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(kind: EffectKind, m: &str) -> EffectRequest {
        EffectRequest::new(kind, m, "pid:1")
    }

    #[test]
    fn policy_first_match_then_default() {
        let p = Policy::parse("# demo\ndefault deny\nallow input click\nallow capture\ndeny * *\n")
            .unwrap();
        assert_eq!(
            p.approve(&r(EffectKind::Input, "click")),
            AdmissionDecision::Allow
        );
        assert!(matches!(
            p.approve(&r(EffectKind::Input, "type")),
            AdmissionDecision::Deny { .. }
        ));
        assert_eq!(
            p.approve(&r(EffectKind::Capture, "capture")),
            AdmissionDecision::Allow
        );
        let open = Policy::parse("default allow\ndeny process\n").unwrap();
        assert!(matches!(
            open.approve(&r(EffectKind::Process, "launch")),
            AdmissionDecision::Deny { .. }
        ));
        assert_eq!(
            open.approve(&r(EffectKind::Input, "key")),
            AdmissionDecision::Allow
        );
    }

    #[test]
    fn policy_rejects_ambiguity() {
        assert!(Policy::parse("allow input\n").is_err(), "no default");
        assert!(Policy::parse("default maybe\n").is_err());
        assert!(Policy::parse("default deny\nallow teleport\n").is_err());
        assert!(Policy::parse("default deny\ndefault allow\n").is_err());
    }

    #[test]
    fn global_options_and_env() {
        let a: Vec<String> = ["--policy", "p.txt", "--events", "click", "1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (cfg, rest) = parse_global(&a, |_| None).unwrap();
        assert_eq!(cfg.policy, Some(PathBuf::from("p.txt")));
        assert!(cfg.events);
        assert_eq!(rest, ["click", "1"]);
        let (cfg, rest) = parse_global(&["type".to_string()], |k| {
            (k == "RIGHTKIT_CONTROL_POLICY").then(|| "env.txt".to_string())
        })
        .unwrap();
        assert_eq!(cfg.policy, Some(PathBuf::from("env.txt")));
        assert!(!cfg.events);
        assert_eq!(rest, ["type"]);
        assert!(parse_global(&["--policy".to_string()], |_| None).is_err());
    }

    #[test]
    fn unreadable_policy_fails_closed() {
        let cfg = CliConfig {
            policy: Some(PathBuf::from("/nonexistent/rightkit-control-policy")),
            events: false,
        };
        assert!(gate_for(&cfg).is_err());
        assert!(gate_for(&CliConfig::default()).is_ok());
    }
}
