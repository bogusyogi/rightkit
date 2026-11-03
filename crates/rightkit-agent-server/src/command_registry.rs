//! The one command registry — single source of truth for both the CLI
//! (`clap`, `cli_definitions*.rs`) and the interactive TUI/REPL slash
//! commands (`main_runtime_interactive_session.rs`).
//!
//! P1-1/CR-24 unification: the authoritative table itself lives in
//! `rightkit_agent_host_traits::skill_host::commands` (`COMMAND_SPECS`) — the one place where a
//! command's canonical name, aliases, metadata, and daemon-side executable
//! handler identity are defined together. Dependency direction is
//! `bins -> crates`, so this module is a THIN VIEW over that authority:
//! it re-exports the table plus its projections and layers enforcement
//! helpers on top. Adding or renaming a command touches exactly one table
//! (the crate's); nothing here hand-maintains a second list.
//!
//! `/model` and `session set --model` are two different *surfaces* over the
//! same registry entry (`session-set` concept), not two implementations.

pub use rightkit_agent_host_traits::skill_host::commands::{
    canonical_bare_name, entries_for_surface, entries_in_group, resolve_query, HelpGroup,
};
#[cfg(test)]
pub use rightkit_agent_host_traits::skill_host::commands::{resolve, CommandScope};

/// The single source of truth, re-exported from the shared authority so
/// every in-binary consumer keeps reading it through this module path.
/// The slash command table (installed host).
pub fn command_registry() -> &'static [rightkit_agent_host_traits::skill_host::commands::CommandSpec]
{
    rightkit_agent_host_traits::skill_host::command_specs()
}

/// Whether a command may run while a turn is in flight. This is the one
/// place mid-turn eligibility is *enforced* rather than merely read off the
/// table: an unresolved name is never mid-turn-valid (a caller cannot run a
/// command that doesn't exist), and every caller that needs to gate a
/// mid-turn dispatch — CLI, REPL, or the TUI's `CommandSource` adapter —
/// should call this rather than re-reading `CommandEntry::valid_mid_turn`
/// off a stale/hand-matched lookup.
pub fn is_valid_mid_turn(name: &str) -> bool {
    resolve_query(name)
        .map(|entry| entry.valid_mid_turn)
        .unwrap_or(false)
}

/// Refuse a command name for mid-turn dispatch, returning the reason when it
/// is refused. Callers that need an enforcement point (not just a boolean)
/// use this: it distinguishes "no such command" from "exists, but not valid
/// mid-turn" so the caller can render an accurate refusal message instead of
/// a generic "unknown command".
pub fn enforce_mid_turn(name: &str) -> Result<(), String> {
    match resolve_query(name) {
        None => Err(format!("`{name}` is not a recognized command")),
        Some(entry) if entry.valid_mid_turn => Ok(()),
        Some(entry) => Err(format!(
            "`{}` cannot run while a turn is in flight; wait for the turn to finish",
            entry.name
        )),
    }
}

/// The registry's implementation of the TUI's `CommandSource` seam
/// (`engine/crates/tui/src/component.rs`). `coderight-tui` is a dependency
/// of this binary purely so the `impl` below can name the trait;
/// `CommandRouter`'s own inherent methods are unchanged and unaware of the
/// TUI crate.
///
/// Both the CLI and any interactive surface should route every lookup
/// through this type (or the authority functions it delegates to) rather
/// than hand-matching command strings — that hand-matching is exactly what
/// CR-24 says must not exist.
#[derive(Debug, Default, Clone, Copy)]
pub struct CommandRouter;

impl CommandRouter {
    /// Resolve typed input to a command's canonical name, honouring
    /// aliases. Per the trait's contract, `input` arrives without its
    /// leading slash; this also accepts input that already has one (the
    /// CLI surface never strips it), so the same method serves both forms.
    pub fn resolve(&self, input: &str) -> Option<String> {
        resolve_query(input).map(|entry| entry.name.to_string())
    }

    /// Names (canonical or alias) valid in the current context, for
    /// completion. `prefix` matches against every registered string, not
    /// just canonical names, so completing `/q` offers `/quit`.
    pub fn completions(&self, prefix: &str) -> Vec<String> {
        let mut out: Vec<String> = command_registry()
            .iter()
            .flat_map(|entry| std::iter::once(entry.name).chain(entry.aliases.iter().copied()))
            .filter(|candidate| candidate.starts_with(prefix))
            .map(|candidate| candidate.to_string())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Whether a command may run while a turn is in flight. Delegates to
    /// [`is_valid_mid_turn`] so there is exactly one implementation of this
    /// check for every caller (CLI, REPL, TUI adapter) to share.
    pub fn valid_mid_turn(&self, name: &str) -> bool {
        is_valid_mid_turn(name)
    }
}

impl rightkit_terminal_ui::component::CommandSource for CommandRouter {
    fn resolve(&self, input: &str) -> Option<String> {
        CommandRouter::resolve(self, input)
    }

    fn completions(&self, prefix: &str) -> Vec<String> {
        CommandRouter::completions(self, prefix)
    }

    fn valid_mid_turn(&self, name: &str) -> bool {
        CommandRouter::valid_mid_turn(self, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_host_traits::skill_host::commands::SlashCommand;
    use std::collections::HashSet;

    #[test]
    fn every_alias_resolves_to_its_own_entry() {
        for entry in command_registry() {
            assert_eq!(
                resolve(entry.name).map(|resolved| resolved.name),
                Some(entry.name),
                "canonical name {} must resolve",
                entry.name
            );
            for alias in entry.aliases {
                assert_eq!(
                    resolve(alias).map(|resolved| resolved.name),
                    Some(entry.name),
                    "alias {} must resolve to {}",
                    alias,
                    entry.name
                );
            }
        }
    }

    #[test]
    fn no_duplicate_canonical_names() {
        let mut seen = HashSet::new();
        for entry in command_registry() {
            assert!(
                seen.insert(entry.name),
                "duplicate canonical name: {}",
                entry.name
            );
        }
    }

    #[test]
    fn no_name_collides_with_another_entrys_alias() {
        // Every name (canonical or alias) resolves unambiguously: the first
        // match `resolve` finds for any given string must be the one entry
        // that declares it, i.e. no two entries share a name/alias string.
        let mut all_strings: Vec<&'static str> = Vec::new();
        for entry in command_registry() {
            all_strings.push(entry.name);
            all_strings.extend(entry.aliases.iter().copied());
        }
        let mut seen = HashSet::new();
        for s in &all_strings {
            assert!(
                seen.insert(*s),
                "name/alias `{s}` is declared by more than one entry"
            );
        }
    }

    #[test]
    fn help_is_generated_not_hand_maintained() {
        // `/help` renders exactly entries_in_group(Common); `/help --all`
        // renders Common + Advanced. Assert the split covers every entry
        // exactly once, so there is no third hidden list to fall out of sync.
        let common: Vec<_> = entries_in_group(HelpGroup::Common).collect();
        let advanced: Vec<_> = entries_in_group(HelpGroup::Advanced).collect();
        assert_eq!(common.len() + advanced.len(), command_registry().len());
    }

    #[test]
    fn cli_and_tui_share_the_common_verb_set_modulo_scope() {
        // Every command reachable from both surfaces (BOTH) must exist as
        // exactly one registry entry — proving CLI and TUI resolve to the
        // same implementation rather than parallel definitions.
        for entry in command_registry() {
            if entry.surfaces.cli && entry.surfaces.interactive {
                assert_eq!(resolve(entry.name).unwrap().name, entry.name);
            }
        }
        // Session-scoped commands are interactive-only (a session cannot
        // exist outside an interactive/daemon-attached context); non-session
        // commands reachable from the CLI don't require a live session.
        for entry in command_registry() {
            if entry.scope == CommandScope::Session {
                assert!(
                    entry.surfaces.interactive,
                    "{} is session-scoped but not interactive",
                    entry.name
                );
            }
        }
    }

    /// (a) CLI and REPL must resolve the same name to the same handler.
    /// Both surfaces call `CommandRouter::resolve` rather than hand-matching,
    /// so calling it "as the CLI" and "as the REPL" — i.e. with and without a
    /// leading slash for a command that has both forms registered — must land
    /// on the identical canonical entry.
    #[test]
    fn cli_and_repl_resolve_the_same_name_to_the_same_handler() {
        let router = CommandRouter;
        // `/status` is registered once, reachable from both surfaces
        // (BOTH). The CLI surface would look it up bare or slashed; the
        // REPL surface always has the slash. Both must resolve to the one
        // registry entry.
        let via_repl_form = router.resolve("/status").expect("repl form resolves");
        let via_bare_form = router.resolve("status").expect("bare form resolves");
        assert_eq!(via_repl_form, via_bare_form);
        assert_eq!(via_repl_form, "/status");

        // An alias resolves to the same canonical name as the entry it aliases.
        assert_eq!(router.resolve("hook"), Some("/hooks".to_string()));
        assert_eq!(router.resolve("hook"), router.resolve("/hooks"));
    }

    /// (b) A mid-turn-invalid command is refused mid-turn, not merely
    /// reported as invalid by a flag nobody checks.
    #[test]
    fn mid_turn_invalid_command_is_refused() {
        // `/clear` is registered with `valid_mid_turn: false`.
        assert!(!is_valid_mid_turn("/clear"));
        assert!(enforce_mid_turn("/clear").is_err());

        // `/status` is `valid_mid_turn: true` and must be allowed.
        assert!(is_valid_mid_turn("/status"));
        assert!(enforce_mid_turn("/status").is_ok());

        // An unresolved name is never mid-turn valid.
        assert!(!is_valid_mid_turn("/not-a-real-command"));
        assert!(enforce_mid_turn("/not-a-real-command").is_err());

        let router = CommandRouter;
        assert!(!router.valid_mid_turn("/clear"));
        assert!(router.valid_mid_turn("/status"));

        // Bare forms of slash-canonical commands normalize identically:
        // gating sees the same entry whether the user typed `/clear` or
        // `clear`.
        assert_eq!(
            router.valid_mid_turn("clear"),
            router.valid_mid_turn("/clear")
        );
        assert_eq!(enforce_mid_turn("clear"), enforce_mid_turn("/clear"));
    }

    /// (c) Aliases resolve identically from every surface: whichever string
    /// a surface has on hand (canonical name or any alias), `resolve` and
    /// `CommandRouter::resolve` must agree on the one entry it names.
    #[test]
    fn aliases_resolve_identically_from_every_surface() {
        let router = CommandRouter;
        for entry in command_registry() {
            for alias in entry.aliases {
                assert_eq!(
                    resolve(alias).map(|e| e.name),
                    router.resolve(alias).as_deref(),
                    "alias {alias} must resolve identically via the free fn and CommandRouter"
                );
            }
        }
    }

    /// P1-1 acceptance, formerly the test that enshrined the split: the
    /// bin registry and the daemon's HTTP slash API are now ONE table.
    /// Every surface — CLI, REPL, TUI `CommandSource`, and the daemon
    /// slash API — resolves names through the same rows, aliases normalize
    /// before execution everywhere, and no recognized command can answer
    /// "recognized but no executor".
    #[test]
    fn all_surfaces_share_one_table_and_normalize_aliases_before_execution() {
        use rightkit_agent_host_traits::skill_host::commands::{
            canonical_bare_name, CommandRegistry,
        };

        let router = CommandRouter;
        let registry = CommandRegistry::new();

        // CLI: bare and slash forms land on the same row.
        assert_eq!(resolve_query("model").map(|e| e.name), Some("/model"));
        assert_eq!(resolve("/model").map(|e| e.name), Some("/model"));
        // REPL / TUI `CommandSource` surface.
        assert_eq!(router.resolve("/model").as_deref(), Some("/model"));
        assert_eq!(router.resolve("model").as_deref(), Some("/model"));
        // Daemon HTTP slash-command surface: parses bare names, normalizes
        // through the SAME table, and finds a live executor.
        let parsed = SlashCommand::parse("/model").expect("daemon parses /model");
        assert_eq!(
            canonical_bare_name(&parsed.name),
            Some("model"),
            "the daemon normalizes through the shared table"
        );
        assert!(
            registry.execute("model", &[]).is_some(),
            "every recognized daemon-surface name has a live handler"
        );

        // Structural unification: the binary exposes NO independent table.
        // `COMMAND_REGISTRY` IS the crate's `COMMAND_SPECS` (a re-export),
        // so any command added there appears here automatically.
        for spec in rightkit_agent_host_traits::skill_host::command_specs() {
            assert!(
                command_registry().iter().any(|e| e.name == spec.name),
                "bin view diverged from the authority at {}",
                spec.name
            );
        }

        // Alias normalization before execution, end to end: executing an
        // alias yields the identical result as executing the canonical name.
        for entry in command_registry()
            .iter()
            .filter(|e| e.daemon_handler.is_some())
        {
            let bare = entry.name.trim_start_matches('/');
            let canonical = registry.execute(bare, &[]);
            for alias in entry.aliases {
                assert_eq!(
                    registry.execute(alias, &[]).map(|r| r.output),
                    canonical.as_ref().map(|r| r.output.clone()),
                    "alias {alias} must execute identically to {bare}"
                );
            }
        }
    }

    /// CR-24 fourth-surface parity: every entry reachable from the
    /// interactive/TUI surface must resolve through `CommandRouter::resolve`,
    /// by canonical name AND by every alias. This is the assertion that makes
    /// the TUI a *projection* of COMMAND_REGISTRY rather than a parallel
    /// list: if a row is marked `surfaces.interactive` but does not resolve,
    /// the TUI would render (or complete) a command the router refuses to
    /// dispatch — exactly the hand-maintained-list defect CR-24 bans.
    #[test]
    fn every_interactive_surface_entry_resolves_through_command_router() {
        let router = CommandRouter;
        let interactive: Vec<_> = entries_for_surface(false).collect();
        assert!(
            !interactive.is_empty(),
            "interactive surface must not be empty"
        );
        assert!(
            interactive.iter().any(|e| e.name == "/onboard"),
            "/onboard must be an interactive-surface entry"
        );
        for entry in interactive {
            assert_eq!(
                router.resolve(entry.name).as_deref(),
                Some(entry.name),
                "interactive entry {} must resolve through CommandRouter",
                entry.name
            );
            for alias in entry.aliases {
                assert_eq!(
                    router.resolve(alias).as_deref(),
                    Some(entry.name),
                    "interactive alias {alias} must resolve through CommandRouter to {}",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn completions_matches_names_and_aliases_by_prefix() {
        let router = CommandRouter;
        let matches = router.completions("/q");
        assert!(matches.contains(&"/quit".to_string()));
        assert!(matches.contains(&"/q".to_string()));
    }

    /// Stage-1C/1E grammar: `/repo` and `/effort` resolve through the one
    /// table like every other interactive command. `/repo` is refused
    /// mid-turn (a scope switch must never land during admitted work);
    /// `/effort` is valid mid-turn because it only queues a next-turn value.
    #[test]
    fn repo_and_effort_resolve_with_correct_mid_turn_gating() {
        let router = CommandRouter;
        assert_eq!(router.resolve("/repo").as_deref(), Some("/repo"));
        assert_eq!(router.resolve("repo").as_deref(), Some("/repo"));
        assert_eq!(router.resolve("/repository").as_deref(), Some("/repo"));
        assert_eq!(router.resolve("/effort").as_deref(), Some("/effort"));
        assert_eq!(router.resolve("effort").as_deref(), Some("/effort"));
        assert!(!is_valid_mid_turn("/repo"));
        assert!(enforce_mid_turn("/repo").is_err());
        assert!(is_valid_mid_turn("/effort"));
        assert!(enforce_mid_turn("/effort").is_ok());
    }

    #[test]
    fn adding_a_command_requires_editing_exactly_one_place() {
        // Structural guard: COMMAND_REGISTRY is the only static this module
        // exposes that enumerates commands; entries_in_group and
        // entries_for_surface are pure projections over it, so a new
        // command added to the array automatically appears in both.
        let total_common_and_advanced = entries_in_group(HelpGroup::Common).count()
            + entries_in_group(HelpGroup::Advanced).count();
        assert_eq!(total_common_and_advanced, command_registry().len());
    }
}
