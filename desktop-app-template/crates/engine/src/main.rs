//! rightapp-engine: the engine sidecar AND the headless CLI (one binary, one contract).
//!
//!   rightapp-engine --stdio                     framed JSON sidecar the shell supervises
//!   rightapp-engine <command> [--input <json>]  one-shot CLI; JSON on stdout
//! Failure: `{"error":{"code","message"}}` on stdout, non-zero exit (2 = bad input, 3 = unknown cmd).
//! Commands live in `dispatch` so the CLI and the sidecar can never diverge.

use std::io::{self, BufReader, Read};

use rightapp_protocol::{framed, Request, Response, MAX_FRAME_BYTES};
use serde_json::{json, Value};

const USAGE: &str = "usage: rightapp-engine [--stdio] | doctor | echo --input <json> | --version";

/// The single command table. Add product commands here (PLACEHOLDER: only `doctor`/`echo` exist).
fn dispatch(cmd: &str, input: Value) -> Result<Value, (&'static str, String)> {
    match cmd {
        "doctor" => Ok(json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "app_data": std::env::var("RIGHTAPP_APP_DATA").ok(),
        })),
        "echo" => Ok(input),
        other => Err(("unknown_command", format!("unknown command: {other}"))),
    }
}

fn run_stdio() -> i32 {
    let mut reader = BufReader::new(io::stdin().lock());
    let mut out = io::stdout().lock();
    let mut buf = Vec::new();
    loop {
        match framed::read_json_line::<_, Request>(&mut reader, &mut buf, MAX_FRAME_BYTES) {
            Ok(None) => return 0, // stdin closed: graceful shutdown
            Ok(Some(req)) => {
                let response = match dispatch(&req.cmd, req.input) {
                    Ok(v) => Response::ok(req.id, v),
                    Err((code, msg)) => Response::err(req.id, code, msg),
                };
                if framed::write_json_line(&mut out, &response, MAX_FRAME_BYTES).is_err() {
                    return 1;
                }
            }
            Err(framed::FrameError::Io(_)) => return 1,
            // Oversize/malformed frame was consumed; answer with id 0 and keep serving.
            Err(e) => {
                let _ = framed::write_json_line(&mut out, &Response::err(0, "bad_frame", e.to_string()), MAX_FRAME_BYTES);
            }
        }
    }
}

fn fail(code: &str, message: &str, exit: i32) -> i32 {
    println!("{}", json!({ "error": { "code": code, "message": message } }));
    exit
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input: Option<String> = None;
    let mut words = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--input" {
            input = it.next();
        } else {
            words.push(a);
        }
    }
    let code = match words.first().map(String::as_str) {
        None | Some("--stdio") => run_stdio(),
        Some("--version") | Some("-V") => {
            println!("{}", json!({ "version": env!("CARGO_PKG_VERSION") }));
            0
        }
        Some("--help") | Some("-h") | Some("help") => {
            eprintln!("{USAGE}");
            0
        }
        Some(cmd) => {
            // stdin is read only for `--input -`; an unclosed inherited pipe must never block us.
            let raw = match input.as_deref() {
                Some("-") => {
                    let mut s = String::new();
                    match io::stdin().lock().read_to_string(&mut s) {
                        Ok(_) => Some(s),
                        Err(e) => std::process::exit(fail("bad_input", &format!("read stdin: {e}"), 2)),
                    }
                }
                other => other.map(str::to_string),
            };
            let value = match raw {
                Some(s) => match serde_json::from_str(&s) {
                    Ok(v) => v,
                    Err(e) => std::process::exit(fail("bad_input", &format!("input is not JSON: {e}"), 2)),
                },
                None => json!({}),
            };
            match dispatch(cmd, value) {
                Ok(v) => {
                    println!("{v}");
                    0
                }
                Err((code, msg)) => fail(code, &msg, if code == "unknown_command" { 3 } else { 2 }),
            }
        }
    };
    std::process::exit(code);
}
