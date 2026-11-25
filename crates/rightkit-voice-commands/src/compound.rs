//! Compound commands: "open Claude and go to the first chat".
//!
//! A transcript is a compound only when it is not one catalog command by itself and every part, split
//! on "and" or "then", is a command this crate can recognise, an app launch, or an ordinal target
//! ("the first chat"). One part that is none of those leaves the whole transcript unresolved, and an
//! unresolved addressed request is what the host may hand to the agent; nothing is guessed.
//!
//! An ordinal names a thing in the host's world (a chat, a tab, a window), which this crate cannot see.
//! The host supplies a [`TargetResolver`]. A target is acted on only after the resolver verified it and
//! the host read it back ([`CompoundHost::read_back`]); anything else stops the plan at that step.
//! Steps that already ran are not undone.

use crate::command::CommandAction;
use crate::command_recognition::{app_launch_query, recognize_command};

/// Largest ordinal the grammar understands ("tenth").
const MAX_ORDINAL: u8 = 10;
/// Most words a target noun may have ("the first pull request").
const MAX_NOUN_WORDS: usize = 3;
/// Verbs that introduce a target. "open" is not one: it launches an app.
const TARGET_VERBS: &[&[&str]] = &[
    &["go", "to"],
    &["switch", "to"],
    &["select"],
    &["show"],
    &["focus"],
];

/// Which of several things a target names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ordinal {
    /// 1-based position: "first" is `Nth(1)`.
    Nth(u8),
    Last,
}

/// "The first chat": an ordinal and the noun it counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRef {
    pub ordinal: Ordinal,
    /// Lowercase noun phrase as spoken ("chat", "pull request").
    pub noun: String,
}

/// A target the host found and checked is the thing the words named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedTarget {
    /// The host's own identifier for it.
    pub id: String,
    /// What the host reads back before acting (a chat's title).
    pub label: String,
}

/// What a resolver found for a [`TargetRef`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Verified(VerifiedTarget),
    /// There is nothing at that position.
    NotFound,
    /// More than one thing could be meant, or the list is not stable enough to count.
    Ambiguous,
    /// Something is there but could not be checked; the string says why.
    Unverified(String),
}

/// The host's knowledge of its own targets (UI automation, a chat list, a window list). The crate holds
/// none of it.
pub trait TargetResolver {
    fn resolve(&mut self, target: &TargetRef) -> Resolution;
}

/// Everything a plan needs from the host to run.
pub trait CompoundHost: TargetResolver {
    /// Run one recognised command (the host's dispatcher).
    fn run(&mut self, action: &CommandAction) -> Result<(), String>;
    /// Launch an app by its spoken name; the host resolves it against what is installed and fails when
    /// it is not.
    fn launch(&mut self, query: &str) -> Result<(), String>;
    /// Say or show the verified target before it is acted on.
    fn read_back(&mut self, target: &VerifiedTarget);
    /// Act on a verified target.
    fn select(&mut self, target: &VerifiedTarget) -> Result<(), String>;
}

/// One step of a compound.
#[derive(Debug, Clone)]
pub enum Step {
    Command(CommandAction),
    /// The spoken app name, as [`app_launch_query`] returns it.
    Launch(String),
    Select(TargetRef),
}

/// How a plan ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompoundOutcome {
    /// Every step ran.
    Done { steps: usize },
    /// Step `step` (0-based) did not run; `ran` steps before it did. `reason` is for the pill.
    Stopped {
        step: usize,
        ran: usize,
        reason: String,
    },
}

fn ordinal_word(word: &str) -> Option<Ordinal> {
    if word == "last" {
        return Some(Ordinal::Last);
    }
    let n = match word {
        "first" | "1st" => 1,
        "second" | "2nd" => 2,
        "third" | "3rd" => 3,
        "fourth" | "4th" => 4,
        "fifth" | "5th" => 5,
        "sixth" | "6th" => 6,
        "seventh" | "7th" => 7,
        "eighth" | "8th" => 8,
        "ninth" | "9th" => 9,
        "tenth" | "10th" => MAX_ORDINAL,
        _ => return None,
    };
    Some(Ordinal::Nth(n))
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// "go to the first chat" -> `TargetRef { Nth(1), "chat" }`. A verb, an optional "the", an ordinal and a
/// short noun are all required.
pub fn parse_target(text: &str) -> Option<TargetRef> {
    let words = words(text);
    let verb = TARGET_VERBS.iter().find(|verb| {
        words.len() > verb.len() && words.iter().zip(verb.iter()).all(|(w, v)| w == v)
    })?;
    let mut rest = &words[verb.len()..];
    if rest.first().is_some_and(|word| word == "the") {
        rest = &rest[1..];
    }
    let (first, noun) = rest.split_first()?;
    let ordinal = ordinal_word(first)?;
    if noun.is_empty() || noun.len() > MAX_NOUN_WORDS {
        return None;
    }
    Some(TargetRef {
        ordinal,
        noun: noun.join(" "),
    })
}

/// Split on "and" and "then" ("and then" is one separator). Empty parts are dropped.
fn split_parts(transcript: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for word in words(transcript) {
        if word == "and" || word == "then" {
            if !current.is_empty() {
                parts.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(word);
        }
    }
    if !current.is_empty() {
        parts.push(current.join(" "));
    }
    parts
}

fn classify(part: &str) -> Option<Step> {
    if let Some(target) = parse_target(part) {
        return Some(Step::Select(target));
    }
    if let Some(action) = recognize_command(part) {
        return Some(Step::Command(action));
    }
    app_launch_query(part).map(Step::Launch)
}

/// The steps of a compound transcript, or `None` when it is not one: it is a single catalog command, a
/// single part that is not a target, or some part is not understood. A single ordinal target ("go to the
/// first chat") is a plan of one step.
pub fn plan_compound(transcript: &str) -> Option<Vec<Step>> {
    if recognize_command(transcript).is_some() {
        return None;
    }
    let parts = split_parts(transcript);
    let steps: Vec<Step> = parts
        .iter()
        .map(|part| classify(part))
        .collect::<Option<Vec<_>>>()?;
    let is_plan = steps.len() > 1 || matches!(steps.as_slice(), [Step::Select(_)]);
    is_plan.then_some(steps)
}

/// Run a plan against the host, in order. A target step resolves just before it acts (an earlier step may
/// be what makes the list exist), reads the target back, then selects it. The first step that cannot
/// proceed stops the plan.
pub fn run_plan<H: CompoundHost>(host: &mut H, steps: &[Step]) -> CompoundOutcome {
    for (index, step) in steps.iter().enumerate() {
        let result = match step {
            Step::Command(action) => host.run(action),
            Step::Launch(query) => host.launch(query),
            Step::Select(target) => match host.resolve(target) {
                Resolution::Verified(found) => {
                    host.read_back(&found);
                    host.select(&found)
                }
                Resolution::NotFound => Err(format!("no {} at that position", target.noun)),
                Resolution::Ambiguous => {
                    Err(format!("more than one {} could be meant", target.noun))
                }
                Resolution::Unverified(why) => Err(why),
            },
        };
        if let Err(reason) = result {
            return CompoundOutcome::Stopped {
                step: index,
                ran: index,
                reason,
            };
        }
    }
    CompoundOutcome::Done { steps: steps.len() }
}
