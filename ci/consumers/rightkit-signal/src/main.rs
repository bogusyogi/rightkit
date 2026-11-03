// rightkit-signal 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Only the validation paths run: every call below is refused before any Darwin notification,
// named event or watcher thread is created, so no OS signal state is touched and nothing is posted.
use rightkit_signal::{
    post, validate_name, LeaseMonitor, SignalError, Watcher, MAX_NAME_LEN, MAX_WATCHED,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    check("limits", MAX_NAME_LEN == 200 && MAX_WATCHED == 63);

    // Accepted names: reverse-DNS style, letters, digits, '.', '_' and '-'.
    check(
        "reverse-DNS name accepted",
        validate_name("dev.orthic.pulse.hub.show.overview").is_ok(),
    );
    check(
        "mixed punctuation accepted",
        validate_name("a_B-9.z").is_ok(),
    );
    check(
        "longest name accepted",
        validate_name(&"a".repeat(MAX_NAME_LEN)).is_ok(),
    );

    // Refused names, with the offending name carried in the error.
    check(
        "empty name refused",
        validate_name("") == Err(SignalError::InvalidName(String::new())),
    );
    check(
        "space refused",
        validate_name("bad name") == Err(SignalError::InvalidName("bad name".to_string())),
    );
    check("slash refused", validate_name("a/b").is_err());
    check("non-ASCII refused", validate_name("caf\u{e9}").is_err());
    check(
        "over-long name refused",
        validate_name(&"a".repeat(MAX_NAME_LEN + 1)).is_err(),
    );
    check(
        "invalid error text names the rule",
        validate_name("bad name")
            .err()
            .map(|e| e.to_string().contains("1 to 200"))
            == Some(true),
    );

    // post validates before it reaches the platform, so a bad name posts nothing.
    check(
        "post refuses a bad name",
        matches!(post("bad name"), Err(SignalError::InvalidName(_))),
    );

    // Watcher and lease monitor validate before starting a thread.
    check(
        "watcher needs a name",
        matches!(
            Watcher::start(&[], |_, _| {}).err(),
            Some(SignalError::InvalidNames(_))
        ),
    );
    let too_many: Vec<String> = (0..=MAX_WATCHED).map(|i| format!("n{i}")).collect();
    let too_many_refs: Vec<&str> = too_many.iter().map(String::as_str).collect();
    check(
        "watcher refuses more than the limit",
        matches!(
            Watcher::start(&too_many_refs, |_, _| {}).err(),
            Some(SignalError::InvalidNames(_))
        ),
    );
    check(
        "watcher refuses a bad name",
        matches!(
            Watcher::start(&["ok.name", "bad name"], |_, _| {}).err(),
            Some(SignalError::InvalidName(_))
        ),
    );
    check(
        "lease monitor refuses a bad name",
        matches!(
            LeaseMonitor::start("bad name").err(),
            Some(SignalError::InvalidName(_))
        ),
    );
    check(
        "unsupported text",
        SignalError::Unsupported.to_string() == "signals are not available on this platform",
    );

    println!("OK rightkit-signal 0.1.0 (registry consumer, public API)");
}
