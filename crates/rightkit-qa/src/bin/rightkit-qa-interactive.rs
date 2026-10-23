//! `rightkit-qa-interactive`: run a journey command in the logged-on user's interactive desktop
//! session when this process is in session 0 (SSH, service) on Windows.
//!
//! ```text
//!   rightkit-qa-interactive [--interactive] [--timeout-secs N] [--cwd DIR]
//!                           [--env K=V]... [--pass-env NAME]... -- <program> [args...]
//!   rightkit-qa-interactive status          print the session facts and the decision
//!   rightkit-qa-interactive __child <spec>  internal: the wrapper the one-shot task runs
//! ```
//!
//! In an interactive session (or off Windows) the program simply runs, inheriting stdio. In session
//! 0 it fails with the usual clear error unless the run is opted in (`--interactive` or
//! `RIGHTKIT_QA_INTERACTIVE=1`); opted in, it runs through a one-shot Task Scheduler task as the
//! same account (InteractiveToken, no password), streams stdout and stderr, and exits with the
//! program's exit code. See `rightkit_qa::interactive`.
use rightkit_qa::interactive::{
    child_main, decide, opted_in, run_interactive, select_env, session_facts, Decision, Request,
    Schtasks, Stream, OPT_IN_ENV,
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const USAGE: &str = "usage: rightkit-qa-interactive [--interactive] [--timeout-secs N] [--cwd DIR] [--env K=V]... [--pass-env NAME]... -- <program> [args...]\n       rightkit-qa-interactive status";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("__child") {
        match args.get(1) {
            Some(spec) => std::process::exit(child_main(Path::new(spec))),
            None => std::process::exit(rightkit_qa::interactive::WRAPPER_FAILED),
        }
    }
    match real_main(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("rightkit-qa-interactive: {e}");
            std::process::exit(2);
        }
    }
}

fn real_main(args: &[String]) -> Result<i32, String> {
    if args.first().map(String::as_str) == Some("status") {
        let facts = session_facts();
        let opt = opted_in(false, std::env::var(OPT_IN_ENV).ok().as_deref());
        println!("{facts:#?}");
        println!("opted in: {opt}");
        println!("decision: {:?}", decide(&facts, opt));
        return Ok(0);
    }
    let mut flag = false;
    let mut timeout = 1800u64;
    let mut cwd: Option<PathBuf> = None;
    let mut explicit: Vec<(String, String)> = vec![];
    let mut pass: Vec<String> = vec![];
    let mut i = 0;
    let mut rest: Option<&[String]> = None;
    while i < args.len() {
        let value = |i: usize| {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{} needs a value\n{USAGE}", args[i]))
        };
        match args[i].as_str() {
            "--" => {
                rest = Some(&args[i + 1..]);
                break;
            }
            "--interactive" => flag = true,
            "--timeout-secs" => {
                timeout = value(i)?
                    .parse()
                    .map_err(|_| "--timeout-secs needs a number".to_string())?;
                i += 1;
            }
            "--cwd" => {
                cwd = Some(PathBuf::from(value(i)?));
                i += 1;
            }
            "--env" => {
                let kv = value(i)?;
                let (k, v) = kv.split_once('=').ok_or("--env needs K=V")?;
                explicit.push((k.to_string(), v.to_string()));
                i += 1;
            }
            "--pass-env" => {
                pass.push(value(i)?);
                i += 1;
            }
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
        i += 1;
    }
    let rest = rest.filter(|r| !r.is_empty()).ok_or(USAGE)?;
    let (program, prog_args) = (rest[0].clone(), rest[1..].to_vec());

    let facts = session_facts();
    let decision = decide(
        &facts,
        opted_in(flag, std::env::var(OPT_IN_ENV).ok().as_deref()),
    );
    if let Some(why) = decision.refusal() {
        return Err(why);
    }
    match decision {
        Decision::Direct => {
            // windowless: the direct path runs the program in this wrapper's own console (the user's
            // interactive terminal); hiding it would detach the child from that console.
            let mut cmd = std::process::Command::new(&program);
            cmd.args(&prog_args);
            if let Some(c) = &cwd {
                cmd.current_dir(c);
            }
            cmd.envs(explicit.iter().cloned());
            let status = cmd
                .status()
                .map_err(|e| format!("cannot start {program}: {e}"))?;
            Ok(status.code().unwrap_or(1))
        }
        Decision::Relaunch(account) => {
            let cwd_abs = match &cwd {
                Some(c) => Some(std::path::absolute(c).map_err(|e| e.to_string())?),
                None => None,
            };
            let env = select_env(std::env::vars(), &pass, &explicit).map_err(|e| e.to_string())?;
            let req = Request {
                program,
                args: prog_args,
                cwd: cwd_abs,
                env,
                timeout: Duration::from_secs(timeout),
                child_exe: std::env::current_exe().map_err(|e| e.to_string())?,
                account,
                poll: Duration::from_millis(250),
                start_grace: Duration::from_secs(30),
            };
            let (mut out, mut errs) = (std::io::stdout(), std::io::stderr());
            run_interactive(&Schtasks, &req, &mut |s, b| {
                let _ = match s {
                    Stream::Stdout => out.write_all(b).and_then(|_| out.flush()),
                    Stream::Stderr => errs.write_all(b).and_then(|_| errs.flush()),
                };
            })
            .map_err(|e| e.to_string())
        }
        _ => unreachable!("refusals returned above"),
    }
}
