//! S:I5 — truthful command effects.
//!
//! docs/canon/daemon-master-canon-atoms.md's compatibility table requires `/new`, `/clear`, `/reset`,
//! `/rewind`, `/undo`, `/redo` to be distinct, truthful effects rather than
//! synonyms of each other or of quit. Before this module, no such verb set
//! existed in `engine/crates/tui/src/` (confirmed by the prior lane's `rg`
//! search, reported in the A6 wiring note). This module is that registry:
//! a closed set of [`CommandEffect`]s, name/alias resolution, and a short
//! per-effect description for the completion popup (S:I10) to display.
//!
//! What each effect truthfully does is intentionally left to the host
//! (`tui_host.rs`), which owns the session/turn state these effects act on
//! (the daemon session handle, the transcript, the undo/redo ledger). This
//! module's job is only to make the seven verbs distinct, named, and
//! resolvable -- so no two of them can silently collapse into the same
//! behavior the way an unmanaged ad-hoc match arm easily would.

// Canonical interactive-effect grammar (names, aliases, summaries).
/// Semantic effects owned by interactive command grammar. TUI keeps its
/// public effect enum, but names, aliases, descriptions, and resolution
/// project from this canonical metadata instead of a second local grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractiveEffect {
    New,
    Reset,
    Rewind,
    Undo,
    Redo,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct InteractiveEffectSpec {
    pub(crate) effect: InteractiveEffect,
    pub(crate) name: &'static str,
    pub(crate) aliases: &'static [&'static str],
    pub(crate) summary: &'static str,
}

pub(crate) static INTERACTIVE_EFFECT_SPECS: &[InteractiveEffectSpec] = &[
    InteractiveEffectSpec {
        effect: InteractiveEffect::New,
        name: "new",
        aliases: &["new", "clear", "cls"],
        summary: "Start a new session",
    },
    InteractiveEffectSpec {
        effect: InteractiveEffect::Reset,
        name: "reset",
        aliases: &["reset"],
        summary: "Cancel anything in flight and drop uncommitted edits, keeping this session's identity",
    },
    InteractiveEffectSpec {
        effect: InteractiveEffect::Rewind,
        name: "rewind",
        aliases: &["rewind"],
        summary: "Rewind to a prior turn boundary, discarding everything after it",
    },
    InteractiveEffectSpec {
        effect: InteractiveEffect::Undo,
        name: "undo",
        aliases: &["undo"],
        summary: "Undo the single most recent reversible action",
    },
    InteractiveEffectSpec {
        effect: InteractiveEffect::Redo,
        name: "redo",
        aliases: &["redo"],
        summary: "Redo the most recently undone action",
    },
];

pub(crate) fn resolve_interactive_effect(query: &str) -> Option<&'static InteractiveEffectSpec> {
    let normalized = query.trim().trim_start_matches('/').to_ascii_lowercase();
    INTERACTIVE_EFFECT_SPECS
        .iter()
        .find(|spec| spec.aliases.iter().any(|alias| *alias == normalized))
}

/// One of the seven truthful command-effect verbs. `/quit`, `/exit`, `/q`
/// stay composer-level intents (`composer::Intent::Quit`) and are
/// deliberately **not** part of this enum -- docs/canon/daemon-master-canon-atoms.md's preservation column
/// for I5 keeps them as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandEffect {
    /// Start a brand-new session: fresh transcript, fresh timeline, fresh
    /// daemon session handle. Nothing from the old session is reachable
    /// afterward except through history/paging.
    New,
    /// Reset the current session's turn state (cancel anything in flight,
    /// drop uncommitted edits) while keeping the same session identity and
    /// its already-durable history.
    Reset,
    /// Rewind the session to a prior point in its own history (a specific
    /// turn boundary), discarding everything after that point. Distinct
    /// from `Undo`: rewind is coarse-grained (turn boundaries) and
    /// destructive to what follows; undo is fine-grained and reversible.
    Rewind,
    /// Undo the single most recent reversible action.
    Undo,
    /// Redo the most recently undone action, if nothing new has happened
    /// since.
    Redo,
}

impl CommandEffect {
    pub const ALL: [CommandEffect; 5] = [
        CommandEffect::New,
        CommandEffect::Reset,
        CommandEffect::Rewind,
        CommandEffect::Undo,
        CommandEffect::Redo,
    ];

    /// Canonical `/`-less command name.
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    /// One-line description for the completion popup (S:I10).
    pub fn description(self) -> &'static str {
        self.spec().summary
    }

    /// Every alias this effect resolves from, canonical name included.
    ///
    /// The completion popup and the command parser consume this same list so
    /// an alias can never work in one surface while disappearing from another.
    pub fn aliases(self) -> &'static [&'static str] {
        self.spec().aliases
    }

    /// Resolve typed input (without its leading slash) to a distinct
    /// effect. Returns `None` for anything not in the registry -- notably
    /// `quit`/`exit`/`q`, which are truthfully not this registry's concern.
    pub fn resolve(input: &str) -> Option<CommandEffect> {
        let spec = resolve_interactive_effect(input)?;
        Some(match spec.effect {
            InteractiveEffect::New => CommandEffect::New,
            InteractiveEffect::Reset => CommandEffect::Reset,
            InteractiveEffect::Rewind => CommandEffect::Rewind,
            InteractiveEffect::Undo => CommandEffect::Undo,
            InteractiveEffect::Redo => CommandEffect::Redo,
        })
    }

    fn spec(self) -> &'static InteractiveEffectSpec {
        let effect = match self {
            CommandEffect::New => InteractiveEffect::New,
            CommandEffect::Reset => InteractiveEffect::Reset,
            CommandEffect::Rewind => InteractiveEffect::Rewind,
            CommandEffect::Undo => InteractiveEffect::Undo,
            CommandEffect::Redo => InteractiveEffect::Redo,
        };
        INTERACTIVE_EFFECT_SPECS
            .iter()
            .find(|spec| spec.effect == effect)
            .expect("canonical interactive effect metadata missing")
    }
}
