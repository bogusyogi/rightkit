//! Caller identity for engine invocations: `--caller <name>` (else `<PREFIX>_CALLER`) names who is
//! driving the engine so the outputs it produces can be attributed to that caller.
//!
//! The flag is exported as `<PREFIX>_CALLER` by [`apply`], so a detached runner the command
//! starts inherits it. Only the commands the app lists accept the flag; on any other command an
//! explicit flag is a usage error and is never silently ignored.

use crate::wire::EngineError;
use crate::EngineSpec;

/// Caller used when an invocation names none.
pub const DEFAULT_CALLER: &str = "cli";
/// Suffix of the sidecar beside an output file that records its caller: `<file>.caller.json`.
pub const SIDECAR_SUFFIX: &str = ".caller.json";
/// Longest accepted caller name, in characters.
pub const MAX_CALLER_LEN: usize = 40;

/// A caller identity: a stable `slug` consumers filter on and the `label` they show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Lowercase slug (`my-app`).
    pub slug: String,
    /// The name as given (`My App`).
    pub label: String,
}

impl Caller {
    /// The default caller (`cli`).
    pub fn cli() -> Self {
        Self {
            slug: DEFAULT_CALLER.to_string(),
            label: DEFAULT_CALLER.to_string(),
        }
    }
}

/// `My App` becomes slug `my-app`, label `My App`. At most [`MAX_CALLER_LEN`] characters of
/// letters, digits, `-`, `_` and inner spaces (spaces become `-` in the slug). Anything else is
/// refused, so a caller name can never carry a path separator into a file name or JSON field.
pub fn parse_caller(raw: &str) -> Result<Caller, String> {
    let label = raw.trim();
    if label.is_empty() {
        return Err("caller must not be empty".to_string());
    }
    if label.chars().count() > MAX_CALLER_LEN {
        return Err(format!(
            "caller must be at most {MAX_CALLER_LEN} characters"
        ));
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' '))
    {
        return Err(format!(
            "caller {label:?} may contain only letters, digits, '-', '_' and spaces"
        ));
    }
    let slug: String = label.to_ascii_lowercase().replace(' ', "-");
    if !slug.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err("caller must contain a letter or digit".to_string());
    }
    Ok(Caller {
        slug,
        label: label.to_string(),
    })
}

/// The caller for this process: the flag when given, else `<PREFIX>_CALLER`, else `cli`.
pub fn resolve_caller(spec: &EngineSpec, flag: Option<&str>) -> Result<Caller, String> {
    resolve_caller_from(flag, std::env::var(spec.env_name("CALLER")).ok().as_deref())
}

/// Environment-free core of [`resolve_caller`]. An empty value counts as unset.
pub fn resolve_caller_from(flag: Option<&str>, env: Option<&str>) -> Result<Caller, String> {
    match flag.or(env).filter(|v| !v.trim().is_empty()) {
        Some(raw) => parse_caller(raw),
        None => Ok(Caller::cli()),
    }
}

/// The caller this process was explicitly told to use (`<PREFIX>_CALLER`). `Ok(None)` means none
/// was named: nothing is tagged and behaviour is unchanged.
pub fn explicit_caller(spec: &EngineSpec) -> Result<Option<Caller>, String> {
    match std::env::var(spec.env_name("CALLER"))
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        Some(raw) => parse_caller(&raw).map(Some),
        None => Ok(None),
    }
}

/// The caller-related flags of one invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CallerFlags {
    /// `--caller`.
    pub caller: Option<String>,
    /// Flags the app adds (`--budget-project`, ...), by name, in the order given.
    pub extra: Vec<(String, String)>,
}

impl CallerFlags {
    /// Whether any caller flag was given.
    pub fn any(&self) -> bool {
        self.caller.is_some() || !self.extra.is_empty()
    }

    /// The value of the app's extra flag `name`.
    pub fn extra_value(&self, name: &str) -> Option<&str> {
        self.extra
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Remove `--caller <v>` and each `extra_flags` pair (`--budget-project <v>`) from `argv`.
    /// A flag with no value is dropped with value `""` so a later check can refuse it.
    pub fn take(argv: &mut Vec<String>, extra_flags: &[&str]) -> Self {
        let mut flags = Self::default();
        let mut rest = Vec::with_capacity(argv.len());
        let mut iter = std::mem::take(argv).into_iter();
        while let Some(arg) = iter.next() {
            if arg == "--caller" {
                flags.caller = Some(iter.next().unwrap_or_default());
            } else if let Some(name) = extra_flags.iter().find(|f| **f == arg) {
                flags
                    .extra
                    .push(((*name).to_string(), iter.next().unwrap_or_default()));
            } else {
                rest.push(arg);
            }
        }
        *argv = rest;
        flags
    }
}

/// Apply the caller flags to `command`. When `command` is one of `accepting` the named caller (the
/// flag, else `<PREFIX>_CALLER`) is validated and exported as `<PREFIX>_CALLER` so a detached
/// runner inherits it, and returned; `Ok(None)` means none was named. On any other command an
/// explicit flag is a `bad_usage` error.
pub fn apply(
    spec: &EngineSpec,
    command: &str,
    accepting: &[&str],
    flags: &CallerFlags,
) -> Result<Option<Caller>, EngineError> {
    if !accepting.contains(&command) {
        if flags.any() {
            return Err(EngineError::new(
                "bad_usage",
                format!(
                    "--caller and its companion flags apply to {} only",
                    accepting.join(", ")
                ),
            ));
        }
        return Ok(None);
    }
    let name = spec.env_name("CALLER");
    let named = flags
        .caller
        .clone()
        .or_else(|| std::env::var(&name).ok())
        .filter(|c| !c.trim().is_empty());
    let Some(raw) = named else {
        return Ok(None);
    };
    let caller = parse_caller(&raw).map_err(|message| EngineError::new("bad_usage", message))?;
    std::env::set_var(&name, &caller.label);
    Ok(Some(caller))
}
