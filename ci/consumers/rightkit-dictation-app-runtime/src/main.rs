// rightkit-dictation-app-runtime consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; no engine process is spawned.
use rightkit_dictation_app_runtime::engine_process::EngineEvent;
use rightkit_dictation_app_runtime::host::{configure, engine_bin_name, EngineHost, DEFAULT};

fn check(ok: bool, what: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
    }
    ok
}

fn main() {
    let mut all = true;
    all &= check(
        engine_bin_name().starts_with(DEFAULT.binary_stem),
        "default engine binary is the RightKit engine",
    );
    all &= check(
        configure(EngineHost {
            binary_stem: "notewell-engine",
            bundle_identifier: "com.example.notewell",
            app_resource_dir: "notewell",
        }) && !configure(DEFAULT),
        "engine names are configured once",
    );
    let expected = if cfg!(windows) {
        "notewell-engine.exe"
    } else {
        "notewell-engine"
    };
    all &= check(
        engine_bin_name() == expected,
        "binary name follows the configured stem",
    );
    let legacy = serde_json::from_value::<EngineEvent>(serde_json::json!({
        "kind": "session_reset",
        "session_id": "session-1",
        "command_failed": "last insertion unavailable"
    }));
    all &= check(
        matches!(
            legacy,
            Ok(EngineEvent::SessionReset {
                host_command: None,
                command_failed: Some(message),
                ..
            }) if message == "last insertion unavailable"
        ),
        "legacy reset carries its failure without a host command",
    );
    let extended = serde_json::json!({
        "kind": "session_reset",
        "session_id": "session-1",
        "host_command": {"kind": "transform_last_paste", "delivery_id": "delivery-1", "transform": "upper"}
    });
    let event = serde_json::from_value::<EngineEvent>(extended.clone());
    all &= check(
        matches!(
            &event,
            Ok(EngineEvent::SessionReset {
                host_command: Some(_),
                ..
            })
        ) && event.ok().is_some_and(|event| {
            serde_json::to_value(event)
                .is_ok_and(|wire| wire["host_command"] == extended["host_command"])
        }),
        "extended reset retains the typed command in the same session event",
    );
    if !all {
        std::process::exit(1);
    }
}
