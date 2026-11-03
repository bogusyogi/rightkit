// rightkit-agent-server 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// The crate's only public item is `run`, the host binary entry. Calling it starts the agent
// server, installs crash hooks and reads licence locators, and it needs the product's skill
// catalog and skill host. So this consumer names `run` as a function item without calling it.
// The zero-size check proves the item resolves at the pinned exact version. No I/O, no network,
// no process spawn. Exits 1 on a failed check.
fn main() {
    let entry = rightkit_agent_server::run;
    if std::mem::size_of_val(&entry) != 0 {
        eprintln!("FAIL: run is a zero-sized function item");
        std::process::exit(1);
    }
    println!("ok: rightkit_agent_server::run resolves at the pinned exact version");
    println!("OK rightkit-agent-server 0.1.0 (registry consumer, entry not invoked)");
}
