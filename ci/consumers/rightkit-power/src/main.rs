// rightkit-power 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// `Power::keep_awake` and `Activity::begin` change what the system or the process holds awake, so
// neither is called. The watcher only listens: it asks the platform to report power and session state,
// reads what it reports and is dropped. Hosts that cannot start a reader (a headless runner) get a
// `Power` with default conditions and no events, which is the documented behaviour.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rightkit_power::{Ack, Conditions, Event, Power, PowerKind};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    // The unconstrained default: a platform that says nothing never holds work back.
    let none = Conditions::default();
    check(
        "default conditions are unconstrained",
        !none.saver && !none.on_battery && !none.display_off && !none.locked,
    );
    check(
        "keep-awake kind defaults to system",
        PowerKind::default() == PowerKind::System,
    );

    // An acknowledgement handle for an event the system will not wait long for.
    let ack = Ack::default();
    check("ack starts unanswered", !ack.is_done());
    let shared = ack.clone();
    shared.done();
    check("a clone answers for the original", ack.is_done());

    // Events carry their acknowledgement and print without it.
    let suspending = Event::Suspending(Ack::default());
    check(
        "event debug",
        format!("{suspending:?}").starts_with("Suspending"),
    );
    check(
        "event kinds",
        matches!(Event::Resumed, Event::Resumed) && matches!(Event::Locked, Event::Locked),
    );

    // The watcher.
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let power = Power::spawn(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let suspended_hook = Arc::new(AtomicUsize::new(0));
    let hook_counter = Arc::clone(&suspended_hook);
    power.on_suspending(move || {
        hook_counter.fetch_add(1, Ordering::SeqCst);
    });
    let conditions = power.conditions();
    println!(
        "ok: conditions read (saver={} on_battery={} display_off={} locked={})",
        conditions.saver, conditions.on_battery, conditions.display_off, conditions.locked
    );
    let events = power.events();
    println!(
        "ok: {} event(s) queued at start; wake called {} time(s)",
        events.len(),
        wakes.load(Ordering::SeqCst)
    );
    check(
        "no sleep has been reported",
        power.take_suspended_at().is_none(),
    );
    check(
        "the suspend hook has not run",
        suspended_hook.load(Ordering::SeqCst) == 0,
    );
    drop(power);

    println!("OK rightkit-power 0.1.0 (registry consumer, public API)");
}
