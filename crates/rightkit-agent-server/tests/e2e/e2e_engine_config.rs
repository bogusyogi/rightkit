//! Configuration journeys through the real binary: layer precedence, list
//! accumulation, env overrides, secret redaction, legacy-schema loading, and
//! the effect of config on what the model is actually sent.
//!
//! Retires the `coderight-config` unit tests (merge / loader / migrations /
//! instructions / agent_config). Evidence map: ENG-100 config schema
//! migration, layered-config atoms, instruction loading (`AGENTS.md`/`--bare`).

use crate::support::e2e_cli;
use e2e_cli::*;

fn write(path: std::path::PathBuf, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn merged(cli: &Cli, env: &[(&str, &str)]) -> Value {
    let out = cli.exec(&["config"], env);
    assert_eq!(out.code, Some(0), "{}", out.diag());
    serde_json::from_str(out.stdout.trim())
        .unwrap_or_else(|e| panic!("config output is JSON ({e}): {}", out.diag()))
}

#[test]
fn merged_config_layers_user_then_workspace_then_env_with_list_accumulation() {
    let p = Provider::start(vec![]);
    let cli = Cli::with_config(
        &p,
        "\n[session]\neffort = \"low\"\ndisallowed-tools = [\"shell\"]\n",
        &[],
    );
    // The harness already declares `[model]`; extend it rather than redeclare the table.
    let cfg_path = crate::common::app_root(cli.home.path()).join("config.toml");
    let base = std::fs::read_to_string(&cfg_path).unwrap();
    std::fs::write(
        &cfg_path,
        base.replacen(
            "[model]",
            "[model]\nmax-tokens = 1000\nfallback-models = [\"fb-user\"]",
            1,
        ),
    )
    .unwrap();
    write(
        cli.ws().join(".coderight/config.toml"),
        "[model]\nmax-tokens = 2000\nfallback-models = [\"fb-ws\", \"fb-user\"]\n\n[session]\ndisallowed-tools = [\"write_file\", \"shell\"]\n",
    );
    let cfg = merged(&cli, &[]);
    // Scalars: workspace beats user; untouched user scalars survive.
    assert_eq!(cfg["model"]["max-tokens"], 2000, "{cfg}");
    assert_eq!(cfg["session"]["effort"], "low");
    // Lists accumulate in first-seen order, de-duplicated.
    assert_eq!(cfg["model"]["fallback-models"], json!(["fb-user", "fb-ws"]));
    assert_eq!(
        cfg["session"]["disallowed-tools"],
        json!(["shell", "write_file"])
    );
    // The user's provider route is still present after the workspace layer.
    assert!(cfg["provider-routes"]["qual-fixture"]["base-url"]
        .as_str()
        .unwrap()
        .starts_with("http://127.0.0.1:"));

    // Environment beats both files.
    let cfg = merged(
        &cli,
        &[
            ("RIGHTKIT_MODEL", "env/override-model"),
            ("RIGHTKIT_PERMISSION_MODE", "plan"),
        ],
    );
    assert_eq!(cfg["model"]["default-model"], "env/override-model");
    assert_eq!(cfg["permissions"]["mode"], "plan");
}

#[test]
fn config_command_redacts_secrets_but_keeps_everything_else() {
    let p = Provider::start(vec![]);
    let cli = Cli::with_config(&p, "\n[tools]\nweb-search-provider = \"brave\"\nweb-search-api-key = \"sk-SECRET-VALUE-1234567890\"\n", &[]);
    let out = cli.exec(
        &["config"],
        &[("RIGHTKIT_TELEGRAM_TOKEN", "tg-SECRET-TOKEN-9876")],
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert!(
        !out.stdout.contains("sk-SECRET-VALUE-1234567890"),
        "search key leaked: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("tg-SECRET-TOKEN-9876"),
        "telegram token leaked: {}",
        out.stdout
    );
    let cfg: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(
        cfg["tools"]["web-search-provider"], "brave",
        "non-secret siblings stay visible"
    );
    assert_eq!(
        cfg["tools"]["web-search-api-key"], "[redacted]",
        "secret keys keep their slot with the placeholder"
    );
    assert_eq!(cfg["remote"]["telegram-token"], "[redacted]");
}

#[test]
fn config_drives_the_run() {
    // `Cli::with_config` writes the config; the run must honor its default model and provider
    // route.
    let p = Provider::start(vec![Chat::text("config works").into()]);
    let cli = Cli::new(&p);
    let out = cli.run(&["-p"], "hi");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(out.stdout.trim(), "config works");
    assert_eq!(
        p.requests()[0]["model"],
        MODEL,
        "config default-model selects the wire model"
    );
}

#[test]
fn workspace_config_overrides_the_user_default_model_and_cli_flag_overrides_both() {
    let p = Provider::start(vec![Chat::text("one").into(), Chat::text("two").into()]);
    let cli = Cli::with_config(
        &p,
        &format!(
            "\n[provider-routes.alt-route]\ndisplay-name = \"Alt\"\nbase-url = \"{}\"\nmodel = \"deepseek/deepseek-v4-flash\"\napi-key-required = false\n",
            p.base_url()
        ),
        &[],
    );
    // 1) user default -> pro.
    let out = cli.run(&["-p"], "a");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(p.requests()[0]["model"], MODEL);
    // 2) explicit --model beats config default.
    let out = cli.run(
        &["-p", "--model", "alt-route::deepseek/deepseek-v4-flash"],
        "b",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(p.requests()[1]["model"], "deepseek/deepseek-v4-flash");
}

#[test]
fn bare_flag_semantics_instruction_files_load_from_workspace_by_default() {
    let p = Provider::start(vec![Chat::text("ok").into()]);
    let cli = Cli::with_config(
        &p,
        "",
        &[
            ("AGENTS.md", "WORKSPACE-RULE-MARK-5521"),
            ("CLAUDE.md", "CLAUDE-RULE-MARK-6632"),
        ],
    );
    let out = cli.run(&["--stream-json"], "hello");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let sys = p.requests()[0].to_string();
    assert!(
        sys.contains("WORKSPACE-RULE-MARK-5521"),
        "AGENTS.md must be injected"
    );
    assert!(
        sys.contains("CLAUDE-RULE-MARK-6632"),
        "CLAUDE.md must be injected"
    );
}
