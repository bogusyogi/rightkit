// rightkit-research consumed by the exact registry pin, outside the workspace.
// Proves: tool classification separates worker tools from external network effects, and a
// budget account reports its limits as remaining before any use.
use rightkit_research::research_port::effects::{is_external, is_worker};
use rightkit_research::{BudgetAccount, BudgetLimits};
use std::time::Duration;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(is_worker("mcp__host__task"), "mcp task tool is a worker");
    check(
        !is_external("mcp__host__task", None),
        "worker tool is not external",
    );
    check(
        is_external("mcp__host__search", None),
        "other mcp tools are external",
    );
    let limits = BudgetLimits::default();
    let account = BudgetAccount::from_timeout(limits, Duration::from_secs(60));
    check(
        account.remaining().calls == limits.max_calls,
        "fresh budget has every call remaining",
    );
}
