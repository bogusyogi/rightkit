// rightkit-qa 0.10.10 registry consumer, outside the workspace, pinning `rightkit-qa =0.10.10`
// exactly with `credential-owner` and `in-app-shot`.
//
// Proves the pure public surface a suite author uses without a browser, Tauri window, Android
// device, FFmpeg or GUI: `storage::RunKind` parsing and display; the `scenario` run-kind, capture
// and tier rules; `config::{QaConfig::parse, validate_tier_list, TIERS}`; `settings::Settings`
// parsing, lookup and typed reads; `check::{check, Expect}` over a JSON result; and the `util`
// digest, base64 and secret-leak helpers.
// Does not prove: the `control`, `browser`, `android` or `wv2-*` features, `embedded`, `in_app`,
// run and retention paths on disk, scenario execution (`Ctx`, `run_step`), the media and loudness
// validators, or report writing.
// State: in-memory values only. No file, network, process, browser, window or real user data.
use rightkit_qa::check::{check, Expect};
use rightkit_qa::config::{validate_tier_list, QaConfig, TIERS};
use rightkit_qa::credential_owner::{DeclaredSet, ExpectedRunner, OwnerHost, SnapshotStore};
use rightkit_qa::in_app_shot::{parse_args, ShotHost, ShotWindowSpec};
use rightkit_qa::scenario::{
    ensure_capture_allowed, is_capture_method, scenario_kind, tier_selected,
};
use rightkit_qa::settings::Settings;
use rightkit_qa::storage::RunKind;
use rightkit_qa::util::{base64_decode, base64_encode, secret_leaks, sha256_hex};
use serde_json::json;
use std::error::Error;
use std::path::Path;
use std::time::Duration;

const PREVIEW: ShotWindowSpec = ShotWindowSpec {
    label: "preview",
    titles: &["Preview Window"],
    default_file: "preview.png",
};
const SHOT_HOST: ShotHost = ShotHost {
    windows: &[PREVIEW],
    default_window: PREVIEW,
    default_delay: Duration::from_millis(100),
};

const SNAPSHOT_STORE: SnapshotStore = SnapshotStore {
    service: "example.qa-snapshot",
    mac_app: None,
};
static DECLARED: &[DeclaredSet] = &[];

const SAMPLE: &str = r#"
app = "genright"
[env]
GENRIGHT_NO_KEYCHAIN = "1"
[engine]
binary = { darwin = "target/debug/genright-engine", win32 = "target/debug/genright-engine.exe" }
[ui]
binaries = { macos = "a", windows = "b" }
mode = "background"
"#;

fn main() -> Result<(), Box<dyn Error>> {
    let shot = parse_args(&SHOT_HOST, ["--shot=fixture.png", "--shot-window=preview"])?
        .expect("an explicit screenshot request parses");
    assert_eq!(shot.path.as_path(), Path::new("fixture.png"));
    assert_eq!(shot.window_label(), "preview");
    assert_eq!(shot.delay, SHOT_HOST.default_delay);
    assert!(parse_args(&SHOT_HOST, ["--shot", "--shot-window=other"]).is_err());

    let runner = ExpectedRunner::new(None, None)?;
    let invalid_owner = OwnerHost::new("../preview", DECLARED, SNAPSHOT_STORE, "1.0.0", runner);
    assert!(
        matches!(invalid_owner, Err(message) if message == "the app name is not a single path component")
    );

    // Run kinds: parse, display, and refusal of an unknown kind.
    assert_eq!(RunKind::parse(" visual ")?, RunKind::Visual);
    assert_eq!(RunKind::parse("functional")?.as_str(), "functional");
    assert_eq!(RunKind::Visual.to_string(), "visual");
    assert!(
        RunKind::parse("screenshot").is_err(),
        "an unknown run kind is refused"
    );

    // Scenario rules: a visual scenario cannot run inside a functional run, and captures need
    // a visual kind.
    assert_eq!(
        scenario_kind(RunKind::Visual, None, "login")?,
        RunKind::Visual
    );
    assert_eq!(
        scenario_kind(RunKind::Visual, Some(RunKind::Functional), "login")?,
        RunKind::Functional
    );
    assert!(scenario_kind(RunKind::Functional, Some(RunKind::Visual), "login").is_err());
    assert!(ensure_capture_allowed(RunKind::Functional, "login", "screenshot").is_err());
    ensure_capture_allowed(RunKind::Visual, "login", "screenshot")?;
    assert!(is_capture_method("screenshot"));
    assert!(is_capture_method("record_screen"));
    assert!(!is_capture_method("click"));

    // Tiers.
    assert_eq!(TIERS, ["fast", "local-models", "paid"]);
    assert!(tier_selected("fast", "fast,paid"));
    assert!(!tier_selected("paid", "fast"));
    assert!(tier_selected("local-models", "all"));
    validate_tier_list("fast,paid")?;
    assert!(
        validate_tier_list("gold").is_err(),
        "an unknown tier is refused"
    );

    // Configuration parsing: the committed sample parses; an uppercase app key is refused.
    let config = QaConfig::parse(SAMPLE, Path::new("/tmp/x/rightkit-qa.toml"))?;
    assert_eq!(config.app, "genright");
    assert_eq!(config.default_tier, "fast");
    let uppercase = SAMPLE.replace("app = \"genright\"", "app = \"GenRight\"");
    assert!(
        QaConfig::parse(&uppercase, Path::new("/tmp/x/rightkit-qa.toml")).is_err(),
        "an uppercase app key is refused"
    );

    // Settings: `KEY=VALUE` text, typed reads, and fixed pairs.
    let settings =
        Settings::parse("# qa settings\nexport RK_RECIPE_QA_KEY=abc\nRK_RECIPE_QA_RUNS=42\n")?;
    assert_eq!(settings.get("RK_RECIPE_QA_KEY").as_deref(), Some("abc"));
    assert_eq!(settings.get_u64("RK_RECIPE_QA_RUNS")?, Some(42));
    assert!(
        Settings::parse("no equals sign here").is_err(),
        "a line without '=' is refused"
    );
    let pairs = Settings::from_pairs([("RK_RECIPE_QA_MODE", "fast")]);
    assert_eq!(pairs.get_or("RK_RECIPE_QA_MODE", "paid"), "fast");
    assert_eq!(pairs.get_or("RK_RECIPE_QA_ABSENT", "paid"), "paid");

    // Assertions over a JSON result.
    let result = json!({ "status": "ok", "count": 3, "items": ["a", "b"] });
    check(
        &result,
        &Expect {
            path: Some("status".into()),
            eq: Some(json!("ok")),
            ..Default::default()
        },
    )?;
    check(
        &result,
        &Expect {
            path: Some("items".into()),
            len_eq: Some(2),
            ..Default::default()
        },
    )?;
    assert!(
        check(
            &result,
            &Expect {
                path: Some("count".into()),
                gte: Some(4.0),
                ..Default::default()
            },
        )
        .is_err(),
        "a count below the bound fails the check"
    );

    // Utilities: SHA-256 (lowercase hex), standard base64, and secret-leak detection by name.
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(base64_encode(b"qa"), "cWE=");
    assert_eq!(base64_decode("cWE="), Some(b"qa".to_vec()));
    assert!(
        !secret_leaks(b"token=abc123 end", "abc123").is_empty(),
        "a raw secret is found"
    );
    assert!(
        secret_leaks(b"nothing to see", "abc123").is_empty(),
        "a clean output has no leak"
    );

    println!("OK rightkit-qa 0.10.10 (registry consumer, pure public API)");
    Ok(())
}
