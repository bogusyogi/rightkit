// rightkit-instance 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Nothing is hosted and no suite root, socket, named pipe or window is created: `claim`, `claim_in` and
// a valid-id `arbitrate` would make this process a real primary, so they are never called. This consumer
// exercises service naming, the refusal of invalid ids, the message type, the legacy-name derivation, the
// claim options and the in-process destroy hook.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rightkit_instance::instance::{
    arbitrate, service_name, try_service_name, Arbitration, InvalidAppId, InvalidAppIdReason,
    SecondInstance, METHOD,
};
use rightkit_instance::single_instance::{
    ClaimFailure, ClaimOptions, InstanceMessage, LegacySingleInstanceNames, SingleInstance,
    MESSAGE_METHOD,
};
use serde_json::json;

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    check(
        "method names",
        METHOD == "second-instance" && MESSAGE_METHOD == "instance-message",
    );

    // Service names: lower-cased, every character outside [a-z0-9-] becomes '-', then "-shell".
    check(
        "valid id is kept",
        try_service_name("mailright").as_deref() == Ok("mailright-shell"),
    );
    check(
        "dotted id is normalised",
        try_service_name("app.mailright").as_deref() == Ok("app-mailright-shell"),
    );
    check(
        "case and underscore are normalised",
        try_service_name("Mail_Right").as_deref() == Ok("mail-right-shell"),
    );
    check("unchecked name", service_name("A.b") == "a-b-shell");
    check(
        "distinct ids can name one app",
        try_service_name("a.b") == try_service_name("a-b"),
    );

    // Refusals: an empty id, and an id whose service name passes the 48-byte limit.
    check(
        "empty id refused",
        try_service_name("")
            == Err(InvalidAppId {
                app_id: String::new(),
                reason: InvalidAppIdReason::Empty,
            }),
    );
    let longest = "a".repeat(42);
    check("42-byte id accepted", try_service_name(&longest).is_ok());
    let too_long = "a".repeat(43);
    check(
        "43-byte id refused",
        matches!(
            try_service_name(&too_long),
            Err(InvalidAppId {
                reason: InvalidAppIdReason::TooLong {
                    name_len: 49,
                    max: 48
                },
                ..
            })
        ),
    );
    check(
        "invalid id text names the id",
        try_service_name("")
            .err()
            .map(|e| e.to_string().contains("is empty"))
            == Some(true),
    );

    // arbitrate refuses an invalid id before it looks for a suite root or hosts anything.
    let seen = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&seen);
    let outcome = arbitrate(
        "",
        "1.0.0",
        Arc::new(move |_second: SecondInstance| {
            sink.fetch_add(1, Ordering::SeqCst);
        }),
    );
    check(
        "arbitrate refuses an empty id without starting as primary",
        matches!(
            outcome,
            Arbitration::InvalidAppId(InvalidAppId {
                reason: InvalidAppIdReason::Empty,
                ..
            })
        ) && seen.load(Ordering::SeqCst) == 0,
    );

    // The forwarded message.
    let message = InstanceMessage::from_process()
        .with_action("show-section")
        .with_arg("section", "overview")
        .with_arg("count", 3);
    check(
        "message builders",
        !message.argv.is_empty()
            && !message.cwd.is_empty()
            && message.action.as_deref() == Some("show-section")
            && message.args.get("section") == Some(&json!("overview"))
            && message.args.get("count") == Some(&json!(3)),
    );
    let wire = serde_json::to_string(&message).unwrap_or_default();
    let back: Result<InstanceMessage, _> = serde_json::from_str(&wire);
    check("message round trip", back.ok() == Some(message));
    let legacy_wire: InstanceMessage =
        serde_json::from_str(r#"{"argv":["a"],"cwd":"/x","action":null}"#).unwrap_or_default();
    check(
        "args default when absent",
        legacy_wire.args.is_empty() && legacy_wire.argv == ["a"],
    );

    // Names the old tauri-plugin-single-instance used.
    let names = LegacySingleInstanceNames::for_identifier("app.heardright.next");
    check(
        "legacy names",
        names.windows_mutex == "app.heardright.next-sim"
            && names.windows_window_class == "app.heardright.next-sic"
            && names.windows_window_title == "app.heardright.next-siw"
            && names.macos_socket == std::path::PathBuf::from("/tmp/app_heardright_next_si.sock")
            && names.linux_dbus_name == "app.heardright.next.SingleInstance",
    );

    // Claim options and failure text.
    let options = ClaimOptions::with_legacy("app.heardright.next");
    check(
        "legacy probe only",
        options.legacy_identifier.as_deref() == Some("app.heardright.next") && !options.hold_legacy,
    );
    check(
        "holding the legacy claim",
        options.holding_legacy().hold_legacy,
    );
    check(
        "default options",
        ClaimOptions::default().legacy_identifier.is_none(),
    );
    check(
        "failure text",
        ClaimFailure::InvalidId("x".into()).to_string() == "single-instance id is not usable: x",
    );

    // The destroy hook runs once, however many times destroy is called, and no claim is involved.
    let hooks = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hooks);
    SingleInstance::on_destroy(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    SingleInstance::destroy();
    SingleInstance::destroy();
    check(
        "destroy runs a registered hook exactly once",
        hooks.load(Ordering::SeqCst) == 1,
    );

    println!("OK rightkit-instance 0.1.0 (registry consumer, public API)");
}
