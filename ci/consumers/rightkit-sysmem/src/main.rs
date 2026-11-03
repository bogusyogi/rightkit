// rightkit-sysmem 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Calls the three public queries. They are process-local reads: allocator pressure relief only
// returns this process's free pages, the physical-memory probe is a read, and the monitor profile
// lookup is a read. None changes system state, and none panics or fails loudly on any host.
use rightkit_sysmem::{release_free_memory, total_physical_memory, window_monitor_profile};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    // Safe to call repeatedly: releasing again after a release is still a plain byte count.
    let first = release_free_memory();
    let second = release_free_memory();
    println!("ok: release_free_memory returned {first} then {second} bytes");

    // Windows answers with the machine's RAM; every other host answers None (no native call needed).
    let total = total_physical_memory();
    #[cfg(windows)]
    check(
        "windows reports physical memory",
        total.is_some_and(|bytes| bytes >= 64 * 1024 * 1024),
    );
    #[cfg(not(windows))]
    check("non-windows has no native probe", total.is_none());

    // Off Windows there is no monitor ICC lookup; on Windows the lookup may legitimately be empty
    // (no monitor, headless runner), so the call is made and its answer is not constrained.
    let profile = window_monitor_profile();
    #[cfg(not(windows))]
    check("non-windows has no monitor profile", profile.is_none());
    #[cfg(windows)]
    println!("ok: window_monitor_profile answered {}", profile.is_some());

    println!("OK rightkit-sysmem 0.1.0 (registry consumer, public API)");
}
