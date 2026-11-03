//! Faithful port of `src/lib/contracts/arcane/authority.mjs` (S02 — authority
//! identity assertion).
//!
//! Authority identity is asserted by kernel/host evidence per turn, never
//! claimed by model text. Authority never travels inside a payload: a
//! payload that carries an authority-shaped field is a typed refusal, not a
//! silently-ignored field (silently dropping it would let a caller believe
//! it had been honored).

use std::collections::HashMap;

use super::errors::{ArcCode, ArcaneError, Decision};

/// Sources permitted to assert an authority. Deliberately excludes the model.
pub const ASSERTION_SOURCE: &[&str] = &["kernel", "host"];

/// Field names that constitute a self-asserted authority claim in a payload.
pub const PAYLOAD_AUTHORITY_FIELDS: &[&str] = &[
    "authority",
    "callerAuthority",
    "assertedAuthority",
    "trust_class",
    "trustClass",
    "executor",
];

/// Mirrors `packages/contracts/enums.mjs` `AUTHORITY_ID`.
pub const AUTHORITY_ID: &[&str] = &["legion", "sage", "alchemist", "oracle", "arcane", "kernel"];

/// Mirrors `packages/contracts/enums.mjs` `AUTHENTICATION_METHOD`.
pub const AUTHENTICATION_METHOD: &[&str] = &[
    "host-connection-trust",
    "capability-signature",
    "unauthenticated",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    pub turn_id: String,
    pub authority: String,
    pub asserted_by: String,
    pub verification_method: String,
    pub per_message: bool,
    pub source: String,
    pub asserted_at: i64,
}

/// Per-turn record of who the kernel says is acting.
///
/// Deliberately NOT persisted: an authority assertion is scoped to one turn
/// and must be re-made, not recovered.
pub struct AuthorityLedger {
    turns: HashMap<String, Assertion>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl Default for AuthorityLedger {
    fn default() -> Self {
        Self::new(|| 0)
    }
}

pub struct AssertForTurnInput<'a> {
    pub turn_id: &'a str,
    pub authority: &'a str,
    pub asserted_by: &'a str,
    pub verification_method: &'a str,
    pub per_message: bool,
    pub source: &'a str,
}

impl AuthorityLedger {
    pub fn new(clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self {
            turns: HashMap::new(),
            clock: Box::new(clock),
        }
    }

    /// Record the authority for a turn.
    ///
    /// Errors `ARC_AUTHORITY_MODEL_CLAIMED` when the source is not a
    /// permitted asserter, the authority is not a canonical `AUTHORITY_ID`,
    /// the verification method is unknown, or `perMessage` is combined with
    /// `host-connection-trust` (connection trust cannot be per-message).
    pub fn assert_for_turn(
        &mut self,
        input: AssertForTurnInput<'_>,
    ) -> Result<Assertion, ArcaneError> {
        if !ASSERTION_SOURCE.contains(&input.source) {
            return Err(ArcaneError::new(
                ArcCode::ArcAuthorityModelClaimed,
                format!(
                    "authority may only be asserted by {}, not '{}'",
                    ASSERTION_SOURCE.join(" or "),
                    input.source
                ),
            )
            .with_detail("turnId", input.turn_id.to_string())
            .with_detail("source", input.source.to_string()));
        }
        if !AUTHORITY_ID.contains(&input.authority) {
            return Err(ArcaneError::new(
                ArcCode::ArcAuthorityModelClaimed,
                format!("'{}' is not a canonical Legion authority", input.authority),
            )
            .with_detail("turnId", input.turn_id.to_string())
            .with_detail("authority", input.authority.to_string()));
        }
        if !AUTHENTICATION_METHOD.contains(&input.verification_method) {
            return Err(ArcaneError::new(
                ArcCode::ArcAuthorityModelClaimed,
                format!(
                    "unknown verification method '{}'",
                    input.verification_method
                ),
            )
            .with_detail("turnId", input.turn_id.to_string())
            .with_detail("verificationMethod", input.verification_method.to_string()));
        }
        if input.per_message && input.verification_method == "host-connection-trust" {
            return Err(ArcaneError::new(
                ArcCode::ArcAuthorityModelClaimed,
                "host-connection-trust authenticates a connection, not a message — perMessage cannot be true (S00 finding 2)",
            )
            .with_detail("turnId", input.turn_id.to_string())
            .with_detail("verificationMethod", input.verification_method.to_string()));
        }
        let assertion = Assertion {
            turn_id: input.turn_id.to_string(),
            authority: input.authority.to_string(),
            asserted_by: input.asserted_by.to_string(),
            verification_method: input.verification_method.to_string(),
            per_message: input.per_message,
            source: input.source.to_string(),
            asserted_at: (self.clock)(),
        };
        self.turns
            .insert(input.turn_id.to_string(), assertion.clone());
        Ok(assertion)
    }

    /// The assertion for `turn_id`, or `None`.
    pub fn current(&self, turn_id: &str) -> Option<&Assertion> {
        self.turns.get(turn_id)
    }

    /// Drop a turn's assertion (turn ended). Returns whether one existed.
    pub fn clear_turn(&mut self, turn_id: &str) -> bool {
        self.turns.remove(turn_id).is_some()
    }

    pub fn size(&self) -> usize {
        self.turns.len()
    }
}

/// Always errors. Exists so the refusal is a callable, greppable thing:
/// no code path may read an authority out of a payload. Mirrors JS
/// `extractAuthorityFromPayload`.
pub fn extract_authority_from_payload(payload_fields: &[&str]) -> ArcaneError {
    let found: Vec<&str> = PAYLOAD_AUTHORITY_FIELDS
        .iter()
        .copied()
        .filter(|f| payload_fields.contains(f))
        .collect();
    ArcaneError::new(
        ArcCode::ArcAuthorityModelClaimed,
        "authority cannot be read from a payload; it is asserted by kernel or host evidence per turn",
    )
    .with_detail("fields", found.join(","))
}

/// True when a payload (given as its field names) is trying to name its own
/// authority. Mirrors JS `payloadClaimsAuthority`.
pub fn payload_claims_authority(payload_fields: &[&str]) -> bool {
    PAYLOAD_AUTHORITY_FIELDS
        .iter()
        .any(|f| payload_fields.contains(f))
}

#[derive(Default)]
pub struct RequireAuthorityOpts<'a> {
    pub claimed_authority: Option<&'a str>,
    pub require_per_message: bool,
}

/// Gate on the kernel-asserted authority for a turn. Mirrors JS
/// `requireAuthority`.
pub fn require_authority(
    ledger: &AuthorityLedger,
    turn_id: &str,
    allowed: &[&str],
    opts: RequireAuthorityOpts<'_>,
) -> Decision {
    let Some(assertion) = ledger.current(turn_id) else {
        return Decision::deny(
            ArcCode::ArcAuthorityNotAsserted,
            format!("no kernel-asserted authority for turn {turn_id}"),
            vec![("turnId".into(), turn_id.into())],
        );
    };
    if let Some(claimed) = opts.claimed_authority {
        if claimed != assertion.authority {
            return Decision::deny(
                ArcCode::ArcAuthorityModelClaimed,
                "payload claims an authority the kernel did not assert for this turn",
                vec![
                    ("turnId".into(), turn_id.into()),
                    ("claimed".into(), claimed.into()),
                    ("asserted".into(), assertion.authority.clone()),
                ],
            );
        }
    }
    if !allowed.contains(&assertion.authority.as_str()) {
        return Decision::deny(
            ArcCode::ArcAuthorityNotAsserted,
            format!(
                "authority '{}' is not permitted for this operation",
                assertion.authority
            ),
            vec![
                ("turnId".into(), turn_id.into()),
                ("asserted".into(), assertion.authority.clone()),
                ("allowed".into(), allowed.join(",")),
            ],
        );
    }
    if opts.require_per_message && !assertion.per_message {
        return Decision::deny(
            ArcCode::ArcAuthorityNotAsserted,
            "this operation requires per-message authority; only connection-level trust is available",
            vec![("turnId".into(), turn_id.into()), ("verificationMethod".into(), assertion.verification_method.clone())],
        );
    }
    Decision::allow(vec![
        ("turnId".into(), turn_id.into()),
        ("authority".into(), assertion.authority.clone()),
    ])
}

/// Delegates host authority binding to an injected store. Mirrors JS
/// `assertHostAuthorityForTurn`; a `None` store is `ARC_AUTHORITY_NOT_ASSERTED`.
pub fn assert_host_authority_for_turn<F, T>(
    binding_store: Option<&F>,
    input: T,
) -> Result<Assertion, ArcaneError>
where
    F: Fn(T) -> Result<Assertion, ArcaneError>,
{
    match binding_store {
        Some(f) => f(input),
        None => Err(ArcaneError::new(
            ArcCode::ArcAuthorityNotAsserted,
            "host authority binding store is required",
        )),
    }
}
