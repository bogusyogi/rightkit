//! Registry consumer for rightkit-context-core 0.1.0: builds against the published API only.
use rightkit_context_core::{fuse, CrossProviderBudget, FusionBounds, BUDGET_LANE_KINDS, DEFAULT_MAX_ITEMS};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(!BUDGET_LANE_KINDS.is_empty(), "budget lane kinds are published");
    let _budget = CrossProviderBudget::new(4096);
    println!("ok: a cross-provider budget is built from a token ceiling");
    let _ = fuse;
    let _ = std::mem::size_of::<FusionBounds>();
    check(DEFAULT_MAX_ITEMS > 0, "fusion has a positive default item cap");
    println!("rightkit-context-core consumer: all checks passed");
}
