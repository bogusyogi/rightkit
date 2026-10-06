//! `svc_call <suite-root> <service> <app> <method> [json-params]`: one call, JSON to stdout.
use rightkit_service::*;
use serde_json::{json, Value};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 4 {
        eprintln!("usage: svc_call <suite-root> <service> <app> <method> [json-params]");
        std::process::exit(2);
    }
    let suite = Suite::at(&a[0]).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    let params: Value = a
        .get(4)
        .map(|s| serde_json::from_str(s).expect("params json"))
        .unwrap_or_else(|| json!({}));
    match Client::connect(&suite, &a[1], &a[2]).and_then(|c| c.call(&a[3], params)) {
        Ok(v) => println!("{v}"),
        Err(e) => {
            println!("{}", json!({"error": e.to_value()}));
            std::process::exit(1);
        }
    }
}
