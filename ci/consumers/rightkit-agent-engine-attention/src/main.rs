// rightkit-agent-engine-attention 0.1.0 consumed by exact registry pin: host configuration, router
// construction, OpenAPI fragments and quiet hours on an empty workspace under the OS temp directory.
use rightkit_agent_engine_attention::{
    attention_openapi_components, attention_routes, configure, quiet_hours_active, AttentionConfig,
    AttentionRouterState,
};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let cfg = AttentionConfig {
        state_dir: ".consumer".into(),
        ..AttentionConfig::default()
    };
    check(configure(cfg), "configure installs the host config once");
    let _router = attention_routes::<AttentionRouterState>();
    println!("ok: attention_routes builds");
    check(
        !attention_openapi_components().is_empty(),
        "OpenAPI components are published",
    );
    let ws = std::env::temp_dir().join(format!("rk-attention-consumer-{}", std::process::id()));
    check(
        !quiet_hours_active(&ws.to_string_lossy(), 0),
        "no quiet hours without a policy file",
    );
}
