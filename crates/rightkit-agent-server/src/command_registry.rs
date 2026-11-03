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
