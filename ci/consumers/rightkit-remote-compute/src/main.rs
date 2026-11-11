// rightkit-remote-compute 0.2.0 consumed by exact registry pin (default features), outside the workspace.
// Pure decisions only; no provider key, no network, no SSH.
use rightkit_remote_compute::{commands, remote, rental, SettingsWorkerStore, WorkerStore, WORKERS_KEY};
use serde_json::json;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let now = 10_000.0;
    check(
        rental::rental_due(&json!({}), now).is_none(),
        "no limits, never due",
    );
    let long = json!({"rental": {"rented_at_epoch": 1000.0, "max_duration_min": 60.0}});
    check(
        matches!(
            rental::rental_due(&long, now),
            Some(rental::Reason::Duration)
        ),
        "duration limit due",
    );
    let pricey =
        json!({"rental": {"rented_at_epoch": 1000.0, "dph_total": 10.0, "reserved_usd": 1.0}});
    check(
        matches!(
            rental::rental_due(&pricey, now),
            Some(rental::Reason::Budget)
        ),
        "budget limit due",
    );
    check(
        remote::rental_label(&json!({"worker_id": "w1"})) == "rightkit-w1",
        "rental label uses the host namespace",
    );
    check(WORKERS_KEY == "models.rented_workers", "worker records key");
    check(
        commands::COMMANDS.len() == 5
            && commands::COMMANDS.contains(&"studio.models.worker.action")
            && !commands::COMMANDS.contains(&"studio.models.catalog"),
        "the rented-worker commands, and no local model command",
    );
    let store: &dyn WorkerStore = &SettingsWorkerStore;
    let _ = store;
    println!("ok: the settings table is a worker store");
}
