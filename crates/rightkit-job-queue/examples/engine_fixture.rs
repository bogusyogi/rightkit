//! Scriptable engine for supervisor tests. Speaks the stdio contract
//! (`hello`, then `request`/`response`/`event` JSON lines) and misbehaves on cue.
//!
//! Environment: `FX_DIR` (state directory, required), `FX_PLAN` (comma list of
//! per-generation modes; the last repeats). The generation number is the count
//! of lines in `FX_DIR/gen`, appended on every start. When `FX_LOG_METHODS` is set, every
//! request received is appended, before it is answered, to `FX_DIR/methods` as
//! `<generation> <method>` (never its params).
//!
//! Modes: `ok`, `slow_hello:<ms>`, `no_hello`, `crash:<ms>` (exit 9 shortly after
//! hello), `crash_on_call` (exit 9 on method `work`), `wedge:<ms>` (stops reading
//! after the delay; the process stays alive), `reject_set` (refuses `set_*`), `wrong_contract` (advertises contract_version 2).
//! `deaf` (never reads stdin after hello), `slow_set:<ms>` (every `set_*` takes that long to answer).
//! Methods: `env` (the engine's environment as `{"vars": {..}}`), `chatter{n,ms}` (n rounds, `ms` apart, of one unsolicited event and one event tagged with another request's id, then the result), `slow_progress{n,ms}` (n events tagged with the
//! request id, `ms` apart, then the result), `ping`, `echo`, `pid`, `gen`, `set_*` (remembered), `get_state`,
//! `big` (3 MiB result), `notify{n}` (n unsolicited events), `progress{n}`
//! (n events tagged with the request id; result `{"id"}`), `sleep{ms}`,
//! `hang` (never answered), `work`, `junk` (writes a non-JSON stdout line, then
//! answers), `bare_error` (error reply with neither code nor message), `shutdown`.
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

fn main() {
    let dir = std::path::PathBuf::from(std::env::var("FX_DIR").expect("FX_DIR"));
    let plan = std::env::var("FX_PLAN").unwrap_or_else(|_| "ok".into());
    let gen_path = dir.join("gen");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gen_path)
        .unwrap();
    writeln!(f, "{}", std::process::id()).unwrap();
    let n = std::fs::read_to_string(&gen_path).unwrap().lines().count();
    let modes: Vec<&str> = plan.split(',').collect();
    let mode = modes.get(n - 1).or(modes.last()).copied().unwrap_or("ok");
    let (name, arg) = mode
        .split_once(':')
        .map_or((mode, 0u64), |(a, b)| (a, b.parse().unwrap_or(0)));
    eprintln!("fixture gen {n} mode {mode} key=PRIVATE{n}");

    let stdout = std::io::stdout();
    let put = |v: Value| {
        let mut o = stdout.lock();
        writeln!(o, "{v}").unwrap();
        o.flush().unwrap();
    };
    match name {
        "no_hello" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        "slow_hello" => std::thread::sleep(Duration::from_millis(arg)),
        _ => {}
    }
    let contract = if name == "wrong_contract" { 2 } else { 1 };
    put(json!({"type":"hello","engine":"fixture","version":"1.0","contract_version":contract}));
    let started = Instant::now();
    if name == "crash" {
        std::thread::sleep(Duration::from_millis(arg));
        eprintln!("fatal: boom key=PRIVATE{n}");
        std::process::exit(9);
    }

    if name == "deaf" {
        // Never reads stdin: a large request fills the pipe and the shell's write blocks.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    // Reads happen on this thread; `wedge` stops after the delay.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut state = Map::new();
    let log_methods = std::env::var_os("FX_LOG_METHODS").is_some();
    loop {
        if name == "wedge" && started.elapsed() >= Duration::from_millis(arg) {
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        let line = match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(l) => l,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return,
        };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (id, method, params) = (
            req["id"].clone(),
            req["method"].as_str().unwrap_or("").to_string(),
            req["params"].clone(),
        );
        if log_methods {
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("methods"))
                .unwrap();
            writeln!(log, "{n} {method}").unwrap();
        }
        let ok = |result: Value| json!({"type":"response","id":id,"ok":true,"result":result});
        let reply = match method.as_str() {
            "ping" => ok(json!({"pong": true})),
            "echo" => ok(params),
            "pid" => ok(json!({"pid": std::process::id()})),
            "gen" => ok(json!({"gen": n})),
            m if m.starts_with("set_") => {
                if name == "reject_set" {
                    json!({"type":"response","id":id,"ok":false,"error":{"code":"refused","message":"no settings accepted","retryable":false}})
                } else {
                    if name == "slow_set" {
                        std::thread::sleep(Duration::from_millis(arg));
                    }
                    state.insert(m.to_string(), params);
                    ok(json!({}))
                }
            }
            "get_state" => ok(Value::Object(state.clone())),
            "big" => ok(json!({"blob": "b".repeat(3 << 20)})),
            "notify" => {
                for i in 0..params["n"].as_u64().unwrap_or(1) {
                    put(json!({"type":"event","session_id":"s1","event":{"i": i}}));
                }
                ok(json!({}))
            }
            "sleep" => {
                std::thread::sleep(Duration::from_millis(params["ms"].as_u64().unwrap_or(0)));
                ok(json!({}))
            }
            "progress" => {
                for i in 0..params["n"].as_u64().unwrap_or(1) {
                    put(json!({"type":"event","id":id,"event":{"i": i}}));
                }
                ok(json!({"id": id}))
            }
            "env" => {
                ok(json!({"vars": std::env::vars().collect::<std::collections::BTreeMap<_, _>>()}))
            }
            "chatter" => {
                for i in 0..params["n"].as_u64().unwrap_or(1) {
                    std::thread::sleep(Duration::from_millis(params["ms"].as_u64().unwrap_or(0)));
                    put(json!({"type":"event","session_id":"s1","event":{"i": i}}));
                    put(
                        json!({"type":"event","id":id.as_u64().unwrap_or(0) + 1_000_000,"event":{"i": i}}),
                    );
                }
                ok(json!({"id": id}))
            }
            "slow_progress" => {
                for i in 0..params["n"].as_u64().unwrap_or(1) {
                    std::thread::sleep(Duration::from_millis(params["ms"].as_u64().unwrap_or(0)));
                    put(json!({"type":"event","id":id,"event":{"i": i}}));
                }
                ok(json!({"id": id}))
            }
            "junk" => {
                {
                    let mut o = stdout.lock();
                    writeln!(o, "not json {{").unwrap();
                    o.flush().unwrap();
                }
                ok(json!({"after": true}))
            }
            "bare_error" => json!({"type":"response","id":id,"ok":false,"error":{}}),
            "hang" => continue,
            "work" => {
                if name == "crash_on_call" {
                    eprintln!("fatal: boom key=PRIVATE{n}");
                    std::process::exit(9);
                }
                ok(json!({"worked": true}))
            }
            "shutdown" => {
                put(ok(json!({})));
                return;
            }
            other => {
                json!({"type":"response","id":id,"ok":false,"error":{"code":"unknown_method","message":other,"retryable":false}})
            }
        };
        put(reply);
    }
}
