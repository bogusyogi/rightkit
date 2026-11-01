// rightkit-agent-plugins 0.1.0 consumed by exact registry pin: manifest parsing. No I/O.
use rightkit_agent_plugins::PluginManifest;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let ok = PluginManifest::from_json(
        r#"{"id":"p","name":"P","version":"1.0.0","description":"d","entry":"main.js"}"#,
    );
    check(
        ok.map(|m| m.id == "p").unwrap_or(false),
        "minimal manifest parses",
    );
    check(
        PluginManifest::from_json("not json").is_err(),
        "junk is refused",
    );
}
