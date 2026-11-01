// rightkit-agent-engine-api-integrations 0.1.0 consumed by exact registry pin: plugin discovery over an
// empty directory under the OS temp directory.
use rightkit_agent_engine_api_integrations::discovery::discover_plugins;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let dir = std::env::temp_dir().join(format!("rk-plugins-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    check(
        discover_plugins(&dir).map(|p| p.is_empty()).unwrap_or(true),
        "an empty directory has no plugins",
    );
    let _ = std::fs::remove_dir_all(&dir);
}
