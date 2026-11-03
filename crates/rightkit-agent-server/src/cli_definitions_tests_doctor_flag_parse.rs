// Sibling parse tests for the top-level Command::Doctor flags
// (`--json`, `--verbose`, `--clipboard`). Included from `cli_definitions.rs`
// alongside the other tests_*.rs files.
//
// `Cli` and `Command` are in scope at this include!-into-cli_definitions.rs
// site because `cli_definitions_command.rs` (which defines both) is also
// include!-d into the same parent module. The helper is named uniquely to
// avoid colliding with `parse` defined in any other tests_*.rs sibling at
// the same scope.
//
// Tests are declared at top level (no inner `mod`) because sub-mods don't
// see outer-scope `use`-less imports, which would force a fully qualified
// `crate::cli_definitions_command::Command` path that doesn't exist as a
// top-level module.

fn parse_doctor_flags(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).expect("parse")
}

#[test]
fn doctor_top_level_flags_all_three_enabled() {
    let cli = parse_doctor_flags(&["coderight", "doctor", "--json", "--verbose", "--clipboard"]);
    match cli.command {
        Command::Doctor {
            json,
            verbose,
            clipboard,
        } => {
            assert!(json, "--json should be true");
            assert!(verbose, "--verbose should be true");
            assert!(clipboard, "--clipboard should be true");
        }
        other => panic!("expected top-level doctor (with all flags), got {other:?}"),
    }
}

#[test]
fn doctor_top_level_flags_default_to_false() {
    let cli = parse_doctor_flags(&["coderight", "doctor"]);
    match cli.command {
        Command::Doctor {
            json,
            verbose,
            clipboard,
        } => {
            assert!(!json);
            assert!(!verbose);
            assert!(!clipboard);
        }
        other => panic!("expected top-level doctor (no flags), got {other:?}"),
    }
}

#[test]
fn doctor_top_level_short_verbose_and_clipboard() {
    let cli = parse_doctor_flags(&["coderight", "doctor", "-v", "-c"]);
    match cli.command {
        Command::Doctor {
            json,
            verbose,
            clipboard,
        } => {
            assert!(!json);
            assert!(verbose, "-v should set verbose=true");
            assert!(clipboard, "-c should set clipboard=true");
        }
        other => panic!("expected top-level doctor (-v -c), got {other:?}"),
    }
}
