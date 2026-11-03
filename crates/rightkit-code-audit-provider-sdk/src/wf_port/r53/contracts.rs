//! Port of the four symbols `providers/sdk/index.mjs` re-exports from
//! `src/registry/provider-contracts.mjs`: `PROVIDER_PHASES`,
//! `PROVIDER_ROLES`, `PROVIDER_STATUS`, `assertEnum`.

/// Mirrors `PROVIDER_STATUS`. Closed set, order matches JS exactly.
pub const PROVIDER_STATUS: &[&str] = &[
    "pass",
    "fail",
    "partial",
    "unproven",
    "skipped",
    "error",
    "pending",
    "missing",
    "candidates",
    "blocked",
];

/// Mirrors `PROVIDER_ROLES`.
pub const PROVIDER_ROLES: &[&str] = &[
    "deterministic",
    "model-builder",
    "candidate-generator",
    "hypothesis-generator",
    "adjudicator",
    "variant-analyzer",
    "evidence-synthesizer",
];

/// Mirrors `PROVIDER_PHASES`.
pub const PROVIDER_PHASES: &[&str] = &[
    "facts",
    "model",
    "runtime",
    "hypothesis",
    "reasoning",
    "variants",
    "synthesis",
];

/// Port of `assertEnum(name, value, allowed)`: `Err` carries the same
/// message shape as the thrown JS `Error`.
pub fn assert_enum<'a>(name: &str, value: &'a str, allowed: &[&str]) -> Result<&'a str, String> {
    if !allowed.contains(&value) {
        return Err(format!(
            "{name} must be one of {}; got {:?}",
            allowed.join(", "),
            value
        ));
    }
    Ok(value)
}
