//! The state and the serde hooks behind [`set_retired_config_keys`]; the behaviour is documented
//! there.

use serde::de::{Deserialize, Deserializer, IgnoredAny};

use crate::types::{BudgetConfig, MemoryConfig, WorkflowImportTrustedPublicKey};

/// The keys a host may retire with [`set_retired_config_keys`], as dotted kebab-case TOML paths.
/// `memory` is a whole table; the others are single keys.
pub const RETIRABLE_CONFIG_KEYS: [&str; 7] = [
    "memory",
    "features.workflows-enabled",
    "features.workflow-import-trusted-public-keys",
    "orchestration.budget.global-monthly-max-usd",
    "orchestration.budget.warning-at",
    "orchestration.budget.max-jury-calls-per-run",
    "orchestration.budget.max-escalation-depth",
];

static RETIRED: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();

/// The entry of [`RETIRABLE_CONFIG_KEYS`] that is exactly `key`, if there is one.
fn retirable(key: &str) -> Option<&'static str> {
    RETIRABLE_CONFIG_KEYS
        .iter()
        .copied()
        .find(|known| *known == key)
}

/// Retire configuration keys for this host (once per process): keys of [`HostConfig`] that exist
/// for other hosts and that this host's contracts say do not exist.
///
/// `keys` are entries of [`RETIRABLE_CONFIG_KEYS`], spelled exactly as listed (kebab-case,
/// dotted; matching is on whole path segments, so `orchestration.budget.warning-at` never retires
/// `orchestration.budget.warning-at-something`, and `memory` retires the whole `[memory]` table
/// however a file writes it: a `[memory]` table, an inline table or dotted keys). Naming a key
/// twice is the same as naming it once. Call it at start-up, before anything reads configuration,
/// as the other host setters; what was read before the call stays as it was read.
///
/// Returns `false`, and retires nothing, when a retirement was already set or when any entry of
/// `keys` is not in [`RETIRABLE_CONFIG_KEYS`] (a key this crate has no field-level hook for would
/// otherwise be silently kept). Until it is called with `true`, [`retired_config_keys`] is empty
/// and the crate behaves exactly as without this function.
///
/// From then on a retired key behaves as if its field were not in the configuration types:
///
/// - **Reading**: the key may be present in a layer file (the hardened and the default loader,
///   [`ConfigLoader::load_user_file`](crate::ConfigLoader::load_user_file),
///   [`ConfigLoader::load_str`](crate::ConfigLoader::load_str)), in a `-c key=value` override
///   ([`cli_flags::set_overrides`](crate::cli_flags::set_overrides)) or in any other document read
///   into a [`HostConfig`]; its value is not read (not even type-checked) and the field keeps the
///   value of `Default`.
/// - **Writing**: no serialiser writes it
///   ([`ConfigLoader::save_user`](crate::ConfigLoader::save_user),
///   [`ConfigLoader::save_workspace`](crate::ConfigLoader::save_workspace),
///   [`ConfigLoader::update_user`](crate::ConfigLoader::update_user), `toml` or `serde_json` of a
///   `HostConfig` or of its `features` and `orchestration.budget` tables).
/// - **Unknown keys**: the loader finds a layer's unknown keys by comparing its raw keys with the
///   keys the parsed configuration serialises, so a retired key that a file carries is reported by
///   [`ConfigLoader::unknown_keys`](crate::ConfigLoader::unknown_keys) exactly as a misspelt key
///   is (a retired table is reported as the one key `memory`, as an unknown table is), and a host
///   that refuses unknown keys under `--strict-config` refuses it.
/// - **Schema**: [`config_json_schema`](crate::schema::config_json_schema) (and so
///   `config_json_schema_text`) omits it.
/// - **Validation**: [`HostConfig::validate`](crate::HostConfig::validate) reports nothing about it.
/// - **Merging**: [`merge_with_report`](crate::merge_with_report) lists no conflict and no
///   provenance entry for it.
///
/// The retirement is made by serde field attributes on the types, not by a pass over a parsed
/// document, because host code reads and writes the types directly (for example
/// `serde_json::to_value(&config.features)`) and a pass inside the loader would miss those paths.
/// That is also why only the keys in [`RETIRABLE_CONFIG_KEYS`] can be retired.
///
/// [`HostConfig`]: crate::HostConfig
pub fn set_retired_config_keys(keys: &[&str]) -> bool {
    let mut retired = Vec::with_capacity(keys.len());
    for key in keys {
        let Some(known) = retirable(key) else {
            return false;
        };
        if !retired.contains(&known) {
            retired.push(known);
        }
    }
    RETIRED.set(retired).is_ok()
}

/// The keys this process retired with [`set_retired_config_keys`], each once, in the order the
/// call gave them; empty when none was set.
pub fn retired_config_keys() -> &'static [&'static str] {
    match RETIRED.get() {
        Some(keys) => keys.as_slice(),
        None => &[],
    }
}

/// Whether `path` is a retired key or lies inside a retired table. `path` may spell its
/// segments with `-` or `_` (the Rust field names the validation messages use); matching is on
/// whole segments.
pub(crate) fn is_retired(path: &str) -> bool {
    retired_config_keys()
        .iter()
        .any(|key| covers(key.as_bytes(), path.as_bytes()))
}

/// `key` (kebab-case, dotted) equals `path` or is a leading whole-segment prefix of it, with `_`
/// in `path` read as `-`.
fn covers(key: &[u8], path: &[u8]) -> bool {
    path.len() >= key.len()
        && key
            .iter()
            .zip(path)
            .all(|(wanted, found)| wanted == found || (*wanted == b'-' && *found == b'_'))
        && (path.len() == key.len() || path[key.len()] == b'.')
}

/// Read a retired field's value: skipped without being read or checked when `key` is retired
/// (the field takes `default()`), read as the type always reads otherwise.
fn read_unless_retired<'de, D, T>(
    key: &str,
    deserializer: D,
    default: impl FnOnce() -> T,
) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    if is_retired(key) {
        IgnoredAny::deserialize(deserializer)?;
        Ok(default())
    } else {
        T::deserialize(deserializer)
    }
}

/// The pair of serde hooks of one retirable field: `$read` for `deserialize_with` and `$skip`
/// for `skip_serializing_if`.
macro_rules! retirable_field {
    ($read:ident, $skip:ident, $key:literal, $ty:ty, $default:expr) => {
        pub(crate) fn $read<'de, D: Deserializer<'de>>(deserializer: D) -> Result<$ty, D::Error> {
            read_unless_retired($key, deserializer, || $default)
        }

        pub(crate) fn $skip<T: ?Sized>(_: &T) -> bool {
            is_retired($key)
        }
    };
}

retirable_field!(
    read_memory,
    skip_memory,
    "memory",
    MemoryConfig,
    MemoryConfig::default()
);
retirable_field!(
    read_workflows_enabled,
    skip_workflows_enabled,
    "features.workflows-enabled",
    Option<bool>,
    None
);
retirable_field!(
    read_workflow_import_trusted_public_keys,
    skip_workflow_import_trusted_public_keys,
    "features.workflow-import-trusted-public-keys",
    Vec<WorkflowImportTrustedPublicKey>,
    Vec::new()
);
retirable_field!(
    read_global_monthly_max_usd,
    skip_global_monthly_max_usd,
    "orchestration.budget.global-monthly-max-usd",
    f64,
    BudgetConfig::default().global_monthly_max_usd
);
retirable_field!(
    read_warning_at,
    skip_warning_at,
    "orchestration.budget.warning-at",
    f64,
    BudgetConfig::default().warning_at
);
retirable_field!(
    read_max_jury_calls_per_run,
    skip_max_jury_calls_per_run,
    "orchestration.budget.max-jury-calls-per-run",
    u32,
    BudgetConfig::default().max_jury_calls_per_run
);
retirable_field!(
    read_max_escalation_depth,
    skip_max_escalation_depth,
    "orchestration.budget.max-escalation-depth",
    u32,
    BudgetConfig::default().max_escalation_depth
);
