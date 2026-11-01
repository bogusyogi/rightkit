// rightkit-agent-engine-runtime-middleware 0.1.0 consumed by exact registry pin: builtin registry. No I/O.
use rightkit_agent_engine_runtime_middleware::{
    builtin_middleware, MiddlewarePhase, RuntimeMiddlewareRegistry,
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
    let builtin = builtin_middleware();
    check(!builtin.is_empty(), "builtin_middleware is non-empty");
    let reg = RuntimeMiddlewareRegistry::with_builtin_middleware();
    check(
        reg.middleware().len() == builtin.len(),
        "registry starts with the builtin set",
    );
    let _ = reg.enabled_for_phase(MiddlewarePhase::PreToolCall);
    check(reg.spans(None).is_empty(), "a fresh registry has no spans");
}
