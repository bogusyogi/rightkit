//! The `rightkit-qa run-tests` command line: parsing, and merging the flags with `rightkit-qa.toml`
//! `[run_tests]` and the built-in defaults (flag over file over default).
//!
//! Every argument that is not one of this command's own flags goes to `cargo test --no-run` untouched
//! (`-p fx --test ui --features qa`), and every argument after the first `--` goes to each test binary
//! (`-- --skip slow --exact --nocapture`). Cargo's own `--config` therefore reaches cargo here; this
//! command's file flag is `--qa-config`.
use super::{BuildWith, CwdMode, ListingSource, Options, Settings, Source};
use crate::config::{QaConfig, RunTestsConfig};
use crate::util::{err, Error, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const USAGE: &str = "usage: rightkit-qa run-tests [cargo test arguments] [options] [-- <libtest arguments>]
Compile the test binaries (cargo test --no-run, through `rustright cargo` when it resolves on PATH), then run
them outside the broker and write one receipt. Arguments that are not options below go to cargo.
  --root <dir>              app root: where cargo runs (default .)
  --qa-config <file>        rightkit-qa.toml to read [run_tests] and app from (default <root>/rightkit-qa.toml if present)
  --app <key>               app key for the run storage (default: app in rightkit-qa.toml, else journeys.toml)
  --from-cargo-json <file>  run the executables of an existing cargo JSON listing (- is stdin); compile nothing
  --build-with <how>        auto (default), rustright or cargo
  --require-broker          refuse to compile anywhere but the broker (also [run_tests] require_broker)
  --parallel <n>            binaries run at the same time (default 2)
  --binary-timeout-s <n>    one binary's deadline (default 1800)
  --total-timeout-s <n>     deadline for all binaries together (default 7200)
  --compile-timeout-s <n>   deadline for the compile (default 2700)
  --cwd <how>               package (default), workspace, or a directory relative to --root
  --env <KEY=VALUE>         add to every binary's environment (repeatable)
  --unset-env <KEY>         remove from every binary's environment (repeatable)
  --isolate-tmp             give each binary its own temp directory, removed afterwards
  --no-isolate-tmp          turn [run_tests] isolate_tmp off
  --deny-ignored            fail the run when any test is ignored
  --allow-empty             accept a run in which no test ran
  --show-output             print every binary's output, not only a failed one's tail
  --managed-root <dir>      managed run storage root (default RIGHTKIT_MANAGED_ROOT or the platform default)
  --run-id <id>             name the run directory
  --receipt <file>          also copy the receipt here
exit: 0 passed, 1 a binary failed or timed out (or a run rule failed), 2 setup error or no test binary listed, 3 compile failed";

const VALUE_FLAGS: [&str; 15] = [
    "--root",
    "--qa-config",
    "--app",
    "--managed-root",
    "--run-id",
    "--parallel",
    "--binary-timeout-s",
    "--total-timeout-s",
    "--compile-timeout-s",
    "--env",
    "--unset-env",
    "--cwd",
    "--from-cargo-json",
    "--build-with",
    "--receipt",
];
const BOOL_FLAGS: [&str; 6] = [
    "--require-broker",
    "--deny-ignored",
    "--allow-empty",
    "--isolate-tmp",
    "--no-isolate-tmp",
    "--show-output",
];

/// The command line, parsed and not yet merged with the config file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cli {
    pub root: Option<PathBuf>,
    pub qa_config: Option<PathBuf>,
    pub app: Option<String>,
    pub managed_root: Option<PathBuf>,
    pub run_id: Option<String>,
    pub parallel: Option<usize>,
    pub binary_timeout_s: Option<u64>,
    pub total_timeout_s: Option<u64>,
    pub compile_timeout_s: Option<u64>,
    pub env: Vec<(String, String)>,
    pub unset_env: Vec<String>,
    pub cwd: Option<String>,
    pub from_cargo_json: Option<String>,
    pub build_with: Option<BuildWith>,
    pub require_broker: bool,
    pub deny_ignored: bool,
    pub allow_empty: bool,
    pub isolate_tmp: Option<bool>,
    pub show_output: bool,
    pub receipt: Option<PathBuf>,
    pub cargo_args: Vec<String>,
    /// `Some` once a `--` was seen (even an empty list: it replaces the config's `test_args`).
    pub test_args: Option<Vec<String>>,
}

fn number<T: std::str::FromStr + PartialOrd + From<u8>>(flag: &str, value: &str) -> Result<T> {
    match value.trim().parse::<T>() {
        Ok(n) if n >= T::from(1) => Ok(n),
        _ => err(format!(
            "{flag} {value:?}: expected a whole number of at least 1"
        )),
    }
}

impl Cli {
    fn set_value(&mut self, flag: &str, value: &str) -> Result<()> {
        match flag {
            "--root" => self.root = Some(PathBuf::from(value)),
            "--qa-config" => self.qa_config = Some(PathBuf::from(value)),
            "--app" => self.app = Some(value.to_string()),
            "--managed-root" => self.managed_root = Some(PathBuf::from(value)),
            "--run-id" => self.run_id = Some(value.to_string()),
            "--parallel" => self.parallel = Some(number(flag, value)?),
            "--binary-timeout-s" => self.binary_timeout_s = Some(number(flag, value)?),
            "--total-timeout-s" => self.total_timeout_s = Some(number(flag, value)?),
            "--compile-timeout-s" => self.compile_timeout_s = Some(number(flag, value)?),
            "--env" => match value.split_once('=') {
                Some((k, v)) if !k.is_empty() => self.env.push((k.to_string(), v.to_string())),
                _ => return err(format!("--env {value:?}: expected KEY=VALUE")),
            },
            "--unset-env" => {
                if value.is_empty() || value.contains('=') {
                    return err(format!("--unset-env {value:?}: expected a variable name"));
                }
                self.unset_env.push(value.to_string());
            }
            "--cwd" => {
                if value.trim().is_empty() {
                    return err("--cwd needs package, workspace or a directory");
                }
                self.cwd = Some(value.to_string());
            }
            "--from-cargo-json" => self.from_cargo_json = Some(value.to_string()),
            "--build-with" => self.build_with = Some(BuildWith::parse(value)?),
            "--receipt" => self.receipt = Some(PathBuf::from(value)),
            other => return err(format!("internal: unhandled flag {other}")),
        }
        Ok(())
    }

    fn set_bool(&mut self, flag: &str) {
        match flag {
            "--require-broker" => self.require_broker = true,
            "--deny-ignored" => self.deny_ignored = true,
            "--allow-empty" => self.allow_empty = true,
            "--isolate-tmp" => self.isolate_tmp = Some(true),
            "--no-isolate-tmp" => self.isolate_tmp = Some(false),
            "--show-output" => self.show_output = true,
            _ => {}
        }
    }
}

/// Parse the arguments after `run-tests`.
pub fn parse(args: &[String]) -> Result<Cli> {
    let mut cli = Cli::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--" {
            cli.test_args = Some(args[i..].to_vec());
            break;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n, Some(v)),
            _ => (arg.as_str(), None),
        };
        if VALUE_FLAGS.contains(&name) {
            let value = match inline {
                Some(v) => v.to_string(),
                None => {
                    let v = args
                        .get(i)
                        .ok_or_else(|| Error(format!("{name} needs a value")))?
                        .clone();
                    i += 1;
                    v
                }
            };
            cli.set_value(name, &value)?;
        } else if BOOL_FLAGS.contains(&name) {
            if inline.is_some() {
                return err(format!("{name} takes no value"));
            }
            cli.set_bool(name);
        } else {
            cli.cargo_args.push(arg.clone());
        }
    }
    if cli.from_cargo_json.is_some() {
        if !cli.cargo_args.is_empty() {
            return err(format!(
                "--from-cargo-json compiles nothing, so the cargo arguments {:?} would be ignored; remove them",
                cli.cargo_args
            ));
        }
        if cli.build_with.is_some() {
            return err(
                "--from-cargo-json compiles nothing, so --build-with would be ignored; remove it",
            );
        }
    }
    Ok(cli)
}

fn lowercase_key(app: &str) -> bool {
    app.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && app
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The app key: `--app`, else `app` in `rightkit-qa.toml`, else `app` in `journeys.toml` at the root.
fn resolve_app(flag: Option<&str>, qa: Option<&QaConfig>, root: &Path) -> Result<String> {
    let app = match (flag, qa) {
        (Some(a), _) => Some(a.to_string()),
        (None, Some(q)) => Some(q.app.clone()),
        (None, None) => std::fs::read_to_string(root.join("journeys.toml"))
            .ok()
            .and_then(|t| t.parse::<toml::Table>().ok())
            .and_then(|t| t.get("app").and_then(|v| v.as_str()).map(str::to_string)),
    };
    let Some(app) = app else {
        return err("run-tests needs an app key: pass --app <key>, or have rightkit-qa.toml (app = ...) or journeys.toml at --root");
    };
    if !lowercase_key(&app) {
        return err(format!(
            "app {app:?} is not a lowercase app key (a-z, 0-9 and -, starting with a letter)"
        ));
    }
    Ok(app)
}

fn truthy(value: Option<String>) -> bool {
    value.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Merge the command line, the config file and the defaults into the options of a run. `env` reads
/// `RIGHTKIT_QA_CONFIG` and `RIGHTKIT_QA_REQUIRE_BROKER`.
pub fn options(cli: &Cli, env: &dyn Fn(&str) -> Option<String>) -> Result<Options> {
    let root = cli.root.clone().unwrap_or_else(|| PathBuf::from("."));
    let explicit = cli.qa_config.clone().or_else(|| {
        env("RIGHTKIT_QA_CONFIG")
            .filter(|p| !p.trim().is_empty())
            .map(PathBuf::from)
    });
    let (qa, config_path) = match explicit {
        Some(path) => (Some(QaConfig::load(&path)?), Some(path)),
        None => {
            let path = root.join("rightkit-qa.toml");
            if path.is_file() {
                (Some(QaConfig::load(&path)?), Some(path))
            } else {
                (None, None)
            }
        }
    };
    let cfg = qa.as_ref().map(|q| q.run_tests.clone()).unwrap_or_default();
    let app = resolve_app(cli.app.as_deref(), qa.as_ref(), &root)?;

    let parallel = cli
        .parallel
        .or(cfg.parallel)
        .unwrap_or(RunTestsConfig::DEFAULT_PARALLEL);
    if !(1..=RunTestsConfig::MAX_PARALLEL).contains(&parallel) {
        return err(format!(
            "--parallel {parallel}: expected between 1 and {}",
            RunTestsConfig::MAX_PARALLEL
        ));
    }
    let secs = |flag: Option<u64>, file: Option<u64>, default: u64| {
        Duration::from_secs(flag.or(file).unwrap_or(default))
    };
    let mut env_map: BTreeMap<String, String> = cfg.env.clone();
    env_map.extend(cli.env.iter().cloned());
    let mut unset_env: Vec<String> = cfg.unset_env.clone().unwrap_or_else(|| {
        RunTestsConfig::DEFAULT_UNSET_ENV
            .iter()
            .map(|s| s.to_string())
            .collect()
    });
    for key in &cli.unset_env {
        if !unset_env.contains(key) {
            unset_env.push(key.clone());
        }
    }
    let settings = Settings {
        parallel,
        binary_timeout: secs(
            cli.binary_timeout_s,
            cfg.binary_timeout_s,
            RunTestsConfig::DEFAULT_BINARY_TIMEOUT_S,
        ),
        total_timeout: secs(
            cli.total_timeout_s,
            cfg.total_timeout_s,
            RunTestsConfig::DEFAULT_TOTAL_TIMEOUT_S,
        ),
        compile_timeout: secs(
            cli.compile_timeout_s,
            cfg.compile_timeout_s,
            RunTestsConfig::DEFAULT_COMPILE_TIMEOUT_S,
        ),
        cwd: CwdMode::parse(
            cli.cwd
                .as_deref()
                .or(cfg.cwd.as_deref())
                .unwrap_or("package"),
        ),
        env: env_map,
        unset_env,
        isolate_tmp: cli.isolate_tmp.or(cfg.isolate_tmp).unwrap_or(false),
        deny_ignored: cli.deny_ignored || cfg.deny_ignored.unwrap_or(false),
        allow_empty: cli.allow_empty,
        require_broker: cli.require_broker
            || cfg.require_broker.unwrap_or(false)
            || truthy(env("RIGHTKIT_QA_REQUIRE_BROKER")),
        show_output: cli.show_output,
    };
    let source = match &cli.from_cargo_json {
        Some(path) if path == "-" => Source::Listing(ListingSource::Stdin),
        Some(path) => Source::Listing(ListingSource::File(PathBuf::from(path))),
        None => Source::Compile {
            cargo_args: cli.cargo_args.clone(),
            build_with: cli.build_with.unwrap_or(BuildWith::Auto),
        },
    };
    Ok(Options {
        root,
        app,
        managed_root: cli.managed_root.clone(),
        run_id: cli.run_id.clone(),
        source,
        test_args: cli.test_args.clone().or(cfg.test_args).unwrap_or_default(),
        settings,
        receipt_copy: cli.receipt.clone(),
        config_path,
    })
}

/// The command: parse, merge, run. The result is the process exit code.
pub fn main(args: &[String]) -> Result<i32> {
    let cli = parse(args)?;
    let opts = options(&cli, &|k| std::env::var(k).ok())?;
    Ok(super::run(&opts)?.exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn temp_root(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "rkqa-rtcli-{name}-{}-{}",
            std::process::id(),
            crate::util::new_id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unknown_arguments_go_to_cargo_and_the_rest_after_dashes_go_to_the_binaries() {
        let cli = parse(&argv(
            "-p fx --test ui --features qa --parallel 3 --offline -- --skip slow --exact --nocapture",
        ))
        .unwrap();
        assert_eq!(
            cli.cargo_args,
            argv("-p fx --test ui --features qa --offline")
        );
        assert_eq!(cli.parallel, Some(3));
        assert_eq!(cli.test_args, Some(argv("--skip slow --exact --nocapture")));
    }

    #[test]
    fn a_bare_double_dash_is_an_explicit_empty_list_that_replaces_the_config() {
        assert_eq!(parse(&argv("-p fx --")).unwrap().test_args, Some(vec![]));
        assert_eq!(parse(&argv("-p fx")).unwrap().test_args, None);
    }

    #[test]
    fn own_flags_take_a_value_or_an_equals_form_and_cargo_flags_with_equals_pass_through() {
        let cli = parse(&argv(
            "--app=genright --root=/w --binary-timeout-s=60 --features=a,b --manifest-path=x/Cargo.toml --config=net.offline=true",
        ))
        .unwrap();
        assert_eq!(cli.app.as_deref(), Some("genright"));
        assert_eq!(cli.root, Some(PathBuf::from("/w")));
        assert_eq!(cli.binary_timeout_s, Some(60));
        assert_eq!(
            cli.cargo_args,
            argv("--features=a,b --manifest-path=x/Cargo.toml --config=net.offline=true"),
            "cargo's own --config reaches cargo; this command's file flag is --qa-config"
        );
    }

    #[test]
    fn env_flags_repeat_and_validate() {
        let cli = parse(&argv("--env A=1 --env B=x=y --unset-env C --unset-env D")).unwrap();
        assert_eq!(
            cli.env,
            vec![("A".into(), "1".into()), ("B".into(), "x=y".into())]
        );
        assert_eq!(cli.unset_env, ["C", "D"]);
        for bad in ["--env NOEQUALS", "--env =v", "--unset-env A=B"] {
            assert!(parse(&argv(bad)).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn bad_numbers_missing_values_and_valued_booleans_are_refused() {
        for bad in [
            "--parallel 0",
            "--parallel two",
            "--binary-timeout-s 0",
            "--total-timeout-s -5",
            "--root",
            "--build-with fast",
            "--deny-ignored=yes",
        ] {
            assert!(parse(&argv(bad)).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn a_listing_run_refuses_cargo_arguments_and_build_with_instead_of_ignoring_them() {
        assert!(parse(&argv("--from-cargo-json l.jsonl -p fx")).is_err());
        assert!(parse(&argv("--from-cargo-json l.jsonl --build-with cargo")).is_err());
        assert!(parse(&argv(
            "--from-cargo-json l.jsonl --parallel 1 -- --nocapture"
        ))
        .is_ok());
    }

    #[test]
    fn defaults_apply_with_no_config_file() {
        let root = temp_root("defaults");
        let mut cli = parse(&argv("-p fx")).unwrap();
        cli.root = Some(root.clone());
        cli.app = Some("fx".into());
        let o = options(&cli, &no_env).unwrap();
        assert_eq!(o.app, "fx");
        assert_eq!(o.settings, Settings::default());
        assert!(o.test_args.is_empty());
        assert_eq!(
            o.source,
            Source::Compile {
                cargo_args: argv("-p fx"),
                build_with: BuildWith::Auto
            }
        );
        assert!(o.config_path.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_documented_defaults_hold_with_no_flag_and_no_config() {
        // Literals, not the `RunTestsConfig::DEFAULT_*` constants: this pins what docs/run-tests.md
        // promises (2 binaries at a time, 1800 s per binary, 7200 s in all, 2700 s to compile), so a
        // changed constant fails here and the docs have to change with it.
        let root = temp_root("pinned-defaults");
        let mut cli = parse(&argv("-p fx")).unwrap();
        cli.root = Some(root.clone());
        cli.app = Some("fx".into());
        let o = options(&cli, &no_env).unwrap();
        assert!(o.config_path.is_none());
        assert_eq!(o.settings.parallel, 2);
        assert_eq!(o.settings.binary_timeout, Duration::from_secs(1800));
        assert_eq!(o.settings.total_timeout, Duration::from_secs(7200));
        assert_eq!(o.settings.compile_timeout, Duration::from_secs(2700));
        assert_eq!(o.settings.cwd, CwdMode::Package);
        assert_eq!(
            o.settings.unset_env,
            ["RIGHTKIT_FORWARD_ENV", "RUSTC_WRAPPER"]
        );
        assert!(!o.settings.isolate_tmp && !o.settings.deny_ignored && !o.settings.allow_empty);
        assert!(!o.settings.require_broker);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_app_key_is_required_and_comes_from_a_flag_the_config_or_journeys_toml() {
        let root = temp_root("app");
        let mut cli = Cli {
            root: Some(root.clone()),
            ..Cli::default()
        };
        let e = options(&cli, &no_env).unwrap_err();
        assert!(e.0.contains("needs an app key"), "{e}");
        std::fs::write(root.join("journeys.toml"), "app = \"postright\"\n").unwrap();
        assert_eq!(options(&cli, &no_env).unwrap().app, "postright");
        std::fs::write(root.join("rightkit-qa.toml"), "app = \"fromcfg\"\n").unwrap();
        assert_eq!(options(&cli, &no_env).unwrap().app, "fromcfg");
        cli.app = Some("flag".into());
        assert_eq!(options(&cli, &no_env).unwrap().app, "flag");
        cli.app = Some("Bad_Key".into());
        assert!(options(&cli, &no_env).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_config_file_supplies_defaults_and_the_command_line_wins() {
        let root = temp_root("merge");
        std::fs::write(
            root.join("rightkit-qa.toml"),
            "app = \"genright\"\n[run_tests]\nparallel = 3\nbinary_timeout_s = 600\ntotal_timeout_s = 3000\ntest_args = [\"--test-threads=1\"]\ncwd = \"workspace\"\ndeny_ignored = true\nisolate_tmp = true\nunset_env = [\"ONLY_THIS\"]\n[run_tests.env]\nGENRIGHT_QA = \"1\"\nSHARED = \"from-file\"\n",
        )
        .unwrap();
        // File only.
        let mut cli = parse(&argv("-p genright-e2e")).unwrap();
        cli.root = Some(root.clone());
        let o = options(&cli, &no_env).unwrap();
        assert_eq!(o.app, "genright");
        assert_eq!(o.settings.parallel, 3);
        assert_eq!(o.settings.binary_timeout, Duration::from_secs(600));
        assert_eq!(o.settings.total_timeout, Duration::from_secs(3000));
        assert_eq!(o.settings.compile_timeout, Duration::from_secs(2700));
        assert_eq!(o.settings.cwd, CwdMode::Workspace);
        assert!(o.settings.deny_ignored && o.settings.isolate_tmp);
        assert_eq!(o.settings.unset_env, ["ONLY_THIS"]);
        assert_eq!(o.test_args, ["--test-threads=1"]);
        assert_eq!(
            o.settings.env.get("GENRIGHT_QA").map(String::as_str),
            Some("1")
        );
        assert_eq!(o.config_path, Some(root.join("rightkit-qa.toml")));
        // Flags over file.
        let mut cli = parse(&argv(
            "--parallel 1 --binary-timeout-s 30 --cwd sub --no-isolate-tmp --env SHARED=from-flag --unset-env EXTRA -- --nocapture",
        ))
        .unwrap();
        cli.root = Some(root.clone());
        let o = options(&cli, &no_env).unwrap();
        assert_eq!(o.settings.parallel, 1);
        assert_eq!(o.settings.binary_timeout, Duration::from_secs(30));
        assert_eq!(
            o.settings.total_timeout,
            Duration::from_secs(3000),
            "untouched keys keep the file's value"
        );
        assert_eq!(o.settings.cwd, CwdMode::Path(PathBuf::from("sub")));
        assert!(!o.settings.isolate_tmp);
        assert_eq!(
            o.settings.env.get("SHARED").map(String::as_str),
            Some("from-flag")
        );
        assert_eq!(
            o.settings.env.get("GENRIGHT_QA").map(String::as_str),
            Some("1")
        );
        assert_eq!(o.settings.unset_env, ["ONLY_THIS", "EXTRA"]);
        assert_eq!(
            o.test_args,
            ["--nocapture"],
            "an explicit -- replaces the file's test_args"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_named_config_that_is_missing_is_an_error_but_the_default_one_may_be_absent() {
        let root = temp_root("named");
        let cli = Cli {
            root: Some(root.clone()),
            app: Some("fx".into()),
            qa_config: Some(root.join("nope.toml")),
            ..Cli::default()
        };
        assert!(options(&cli, &no_env).is_err());
        let via_env = options(
            &Cli {
                qa_config: None,
                ..cli.clone()
            },
            &|k| (k == "RIGHTKIT_QA_CONFIG").then(|| root.join("nope.toml").display().to_string()),
        );
        assert!(via_env.is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn require_broker_comes_from_the_flag_the_file_or_the_environment() {
        let root = temp_root("broker");
        let base = Cli {
            root: Some(root.clone()),
            app: Some("fx".into()),
            ..Cli::default()
        };
        assert!(!options(&base, &no_env).unwrap().settings.require_broker);
        assert!(
            options(&base, &|k| (k == "RIGHTKIT_QA_REQUIRE_BROKER")
                .then(|| "1".into()))
            .unwrap()
            .settings
            .require_broker
        );
        let flag = Cli {
            require_broker: true,
            ..base.clone()
        };
        assert!(options(&flag, &no_env).unwrap().settings.require_broker);
        std::fs::write(
            root.join("rightkit-qa.toml"),
            "app = \"fx\"\n[run_tests]\nrequire_broker = true\n",
        )
        .unwrap();
        assert!(options(&base, &no_env).unwrap().settings.require_broker);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parallel_is_bounded_even_when_it_comes_from_the_command_line() {
        let root = temp_root("bound");
        let cli = Cli {
            root: Some(root.clone()),
            app: Some("fx".into()),
            parallel: Some(RunTestsConfig::MAX_PARALLEL + 1),
            ..Cli::default()
        };
        assert!(options(&cli, &no_env).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stdin_is_named_with_a_dash() {
        let root = temp_root("stdin");
        let cli = Cli {
            root: Some(root.clone()),
            app: Some("fx".into()),
            from_cargo_json: Some("-".into()),
            ..Cli::default()
        };
        assert_eq!(
            options(&cli, &no_env).unwrap().source,
            Source::Listing(ListingSource::Stdin)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
