// rightkit-dictation-app-runtime consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; no engine process is spawned.
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
    if !all {
        std::process::exit(1);
    }
}
