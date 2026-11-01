//! Registry consumer for rightkit-remote-gateway 0.1.0: builds against the published API only.
use rightkit_remote_gateway::config::GatewayConfig;
use rightkit_remote_gateway::{execute, manifest, redact, Call, Origin};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        manifest::tools().iter().any(|t| t.name == "files.read"),
        "the bundled tool manifest lists files.read",
    );
    let call = Call {
        client: "consumer".to_string(),
        tool: "no.such.tool".to_string(),
        ..Call::default()
    };
    check(
        execute(&call).err().map(|e| e.code) == Some("unknown_tool"),
        "an unknown tool is refused with a stable code",
    );
    check(
        Origin::Gateway.as_str() == "gateway",
        "origins are named for the activity record",
    );
    check(
        GatewayConfig::default().engine_executable == "rightkit-engine",
        "the default engine name is product-neutral",
    );
    let token = format!("gh{}{}", "p_", "d".repeat(36));
    check(
        redact::redact(&token).1 == 1,
        "redaction masks a token assembled at run time",
    );
    println!("rightkit-remote-gateway consumer: all checks passed");
}
