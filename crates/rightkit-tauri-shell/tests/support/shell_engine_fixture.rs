//! Real NDJSON child for shell policy integration tests.
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn emit(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}
fn main() {
    emit(
        json!({"type": "hello", "engine": "shell-fixture", "contract_version": 1, "pid": std::process::id()}),
    );
    // Every method this process received, in order: `seen` returns them (the composition tests
    // read snapshot replay order from it), `pid` identifies the generation.
    let mut seen: Vec<String> = Vec::new();
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = request["id"].clone();
        let method = request["method"].as_str().unwrap().to_owned();
        let before = seen.clone();
        seen.push(method.clone());
        match method.as_str() {
            "crash" => std::process::exit(7),
            "shutdown" => {
                emit(json!({"type": "response", "ok": true, "id": id, "result": {}}));
                break;
            }
            "seen" => {
                emit(json!({"type": "response", "ok": true, "id": id, "result": before}));
                continue;
            }
            "pid" => {
                emit(
                    json!({"type": "response", "ok": true, "id": id, "result": {"pid": std::process::id()}}),
                );
                continue;
            }
            "notify" => emit(json!({"type": "event", "event": {"notice": "unsolicited"}})),
            "slow" => std::thread::sleep(std::time::Duration::from_millis(150)),
            "slower" => std::thread::sleep(std::time::Duration::from_millis(700)),
            _ => {}
        }
        emit(json!({"type": "event", "id": id, "event": {"stage": "progress"}}));
        emit(json!({"type": "response", "ok": true, "id": id, "result": request["params"]}));
    }
}
