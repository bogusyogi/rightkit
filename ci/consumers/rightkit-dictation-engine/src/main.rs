// rightkit-dictation-engine consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; no engine process, no model.
use rightkit_dictation_engine::file_transcribe::{permission_error, E_PERMISSION};
use rightkit_dictation_engine::guardrail_replay::{replay_guardrails, GuardrailOutcome};
use rightkit_dictation_engine::host::{
    CommandHostIdentity, CrashOptions, EngineIdentity, ProductTerms, ServiceLogNames, WireIdentity,
};
use rightkit_dictation_engine::vocabulary::built_in_terms;
use rightkit_dictation_engine::{configure_host, EngineHostConfig};

fn check(ok: bool, what: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
    }
    ok
}

fn main() {
    let replay = || replay_guardrails("Please ship 12 boxes", "Please ship 12 boxes.", None);
    let before_configuration = replay();
    let mut all = check(
        configure_host(EngineHostConfig {
            app_data_dir_name: "NoteWell",
            app_data_root: Some(std::env::temp_dir().join("notewell-consumer")),
            legal_contract: None,
            disk_settings: None,
            product_terms: ProductTerms {
                built_in: &["NoteWell"],
                core: &["NoteWell"],
            },
            wire_identity: WireIdentity::DEFAULT,
            brand_homophones: None,
            command_host: CommandHostIdentity {
                bundle_id: "com.example.notewell",
                process_name_prefix: "notewell",
                overlay_window_titles: &[],
                display_name: "NoteWell",
            },
            asr_stderr_prefix: "NOTEWELL_ASR_",
            identity: EngineIdentity {
                product_name: "NoteWell",
                ..EngineIdentity::DEFAULT
            },
            cli: None,
            service_logs: ServiceLogNames::DEFAULT,
            crash_version: "1.0.0",
            crash_options: CrashOptions::DEFAULT,
        })
        .is_ok(),
        "one startup configuration sets the engine host and product vocabulary",
    );
    all &= check(
        permission_error(std::path::Path::new("a.wav")).starts_with(E_PERMISSION),
        "facade re-exports file transcription errors",
    );
    all &= check(
        built_in_terms() == ["NoteWell"],
        "facade re-exports the product vocabulary",
    );
    all &= check(
        before_configuration
            == (GuardrailOutcome::Pass {
                accepted_text: "Please ship 12 boxes.".into(),
            })
            && replay() == before_configuration,
        "output acceptance works with or without host vocabulary configured",
    );
    if !all {
        std::process::exit(1);
    }
}
