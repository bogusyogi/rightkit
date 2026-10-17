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
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = request["id"].clone();
        match request["method"].as_str().unwrap() {
            "crash" => std::process::exit(7),
            "shutdown" => {
                emit(json!({"type": "response", "ok": true, "id": id, "result": {}}));
                break;
            }
            "notify" => emit(json!({"type": "event", "event": {"notice": "unsolicited"}})),
            "slow" => std::thread::sleep(std::time::Duration::from_millis(150)),
            _ => {}
        }
        emit(json!({"type": "event", "id": id, "event": {"stage": "progress"}}));
        emit(json!({"type": "response", "ok": true, "id": id, "result": request["params"]}));
    }
}
