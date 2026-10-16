//! The autonomy ladder per (content type, platform). Ported from CutRight's
//! `cutright-rules::ladder` (CutRight #116, RightKit #236) so every app shares one
//! state machine.
//!
//! Every pairing starts at [`Level::Reviewed`]. Each final is recorded with
//! [`Ladder::record`]:
//!
//! - A rejected final drops to `Reviewed` and resets the streak, at any level.
//! - An approved final with no failed critic check extends the streak. At `Reviewed`,
//!   reaching `approvals_to_review_light` promotes to `ReviewLight`. At `ReviewLight`,
//!   `approvals_to_autonomous` more approvals (counted from that promotion) promote to
//!   `Autonomous`.
//! - A failed critic check resets the streak. On an `Autonomous` final it also drops
//!   one level, to `ReviewLight`.
//!
//! The thresholds live in [`LadderPolicy`]. It defaults to 5 and 10, deserialises with
//! those defaults for omitted fields, and is validated on load.
//!
//! The serialised state carries `schema` and `version` ([`LADDER_SCHEMA`],
//! [`LADDER_VERSION`]). A snapshot with neither field is CutRight's original untagged
//! format and is read as version 1.0.0. Any other schema, or a version this build does
//! not read, is refused.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

pub const LADDER_SCHEMA: &str = "rightkit-review.ladder";
/// Schema version `major.minor.patch`. This build reads major 1, minor 0 or older.
pub const LADDER_VERSION: &str = "1.0.0";

/// Autonomy level of one (content type, platform) pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Every final is approved by a reviewer.
    Reviewed,
    /// Approval is needed only for publish and spend-commit classes.
    ReviewLight,
    /// No per-item approval within budget and gate pass.
    Autonomous,
}

/// Promotion thresholds. Overridable; omitted fields keep their defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LadderPolicy {
    /// Clean approved finals at `Reviewed` before promotion to `ReviewLight`.
    pub approvals_to_review_light: u32,
    /// Clean approved finals at `ReviewLight` before promotion to `Autonomous`.
    pub approvals_to_autonomous: u32,
}

impl Default for LadderPolicy {
    fn default() -> Self {
        Self {
            approvals_to_review_light: 5,
            approvals_to_autonomous: 10,
        }
    }
}

impl LadderPolicy {
    /// Parses a policy from JSON, applying defaults to omitted fields, then validates it.
    pub fn from_json(json: &str) -> Result<Self, LadderError> {
        let policy: Self = serde_json::from_str(json)?;
        validate(&policy)?;
        Ok(policy)
    }
}

/// Validation and serialisation failures for the ladder.
#[derive(Debug)]
pub enum LadderError {
    EmptyKey,
    InvalidPolicy(&'static str),
    DuplicateKey(String),
    /// The snapshot names another schema or a version this build cannot read.
    UnsupportedSnapshot { schema: String, version: String },
    Json(serde_json::Error),
}

impl fmt::Display for LadderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LadderError::EmptyKey => {
                write!(f, "ladder key needs a non-empty content type and platform")
            }
            LadderError::InvalidPolicy(field) => {
                write!(f, "ladder policy: {field} must be at least 1")
            }
            LadderError::DuplicateKey(key) => write!(f, "ladder pairing {key} is listed twice"),
            LadderError::UnsupportedSnapshot { schema, version } => {
                write!(f, "unsupported ladder snapshot `{schema}` version {version}")
            }
            LadderError::Json(error) => write!(f, "ladder JSON: {error}"),
        }
    }
}

impl std::error::Error for LadderError {}

impl From<serde_json::Error> for LadderError {
    fn from(error: serde_json::Error) -> Self {
        LadderError::Json(error)
    }
}

/// A (content type, platform) pairing. Both parts are trimmed and must be non-empty.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LadderKey {
    content_type: String,
    platform: String,
}

impl LadderKey {
    pub fn new(content_type: &str, platform: &str) -> Result<Self, LadderError> {
        let content_type = content_type.trim();
        let platform = platform.trim();
        if content_type.is_empty() || platform.is_empty() {
            return Err(LadderError::EmptyKey);
        }
        Ok(Self {
            content_type: content_type.to_owned(),
            platform: platform.to_owned(),
        })
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }
}

/// Level and the count of clean approvals since the last promotion, demotion or reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LadderState {
    pub level: Level,
    pub streak: u32,
}

impl Default for LadderState {
    fn default() -> Self {
        Self {
            level: Level::Reviewed,
            streak: 0,
        }
    }
}

/// The outcome of one final, as the ladder sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Final {
    /// Approved (by a reviewer, or implicitly at `Autonomous`). `critic_failed` marks a
    /// failed critic check on this final.
    Approved { critic_failed: bool },
    /// Rejected by a reviewer.
    Rejected,
}

/// Autonomy ladder state for every pairing that has recorded a final.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ladder {
    policy: LadderPolicy,
    states: BTreeMap<LadderKey, LadderState>,
}

impl Ladder {
    pub fn new(policy: LadderPolicy) -> Result<Self, LadderError> {
        validate(&policy)?;
        Ok(Self {
            policy,
            states: BTreeMap::new(),
        })
    }

    pub fn policy(&self) -> LadderPolicy {
        self.policy
    }

    /// State of a pairing; a pairing with no recorded finals is at its starting state.
    pub fn state(&self, key: &LadderKey) -> LadderState {
        self.states.get(key).copied().unwrap_or_default()
    }

    pub fn level(&self, key: &LadderKey) -> Level {
        self.state(key).level
    }

    /// Applies one final to a pairing and returns its new state.
    pub fn record(&mut self, key: &LadderKey, result: Final) -> LadderState {
        let next = apply(&self.policy, self.state(key), result);
        self.states.insert(key.clone(), next);
        next
    }

    /// Serialises the policy and every touched pairing, in key order, tagged with
    /// [`LADDER_SCHEMA`] and [`LADDER_VERSION`].
    pub fn to_json(&self) -> Result<String, LadderError> {
        let snapshot = Snapshot {
            schema: Some(LADDER_SCHEMA.to_owned()),
            version: Some(LADDER_VERSION.to_owned()),
            policy: self.policy,
            pairings: self
                .states
                .iter()
                .map(|(key, state)| Pairing {
                    content_type: key.content_type.clone(),
                    platform: key.platform.clone(),
                    level: state.level,
                    streak: state.streak,
                })
                .collect(),
        };
        Ok(serde_json::to_string_pretty(&snapshot)?)
    }

    /// Restores a ladder written by [`Ladder::to_json`]. Checks schema and version
    /// first (an untagged snapshot is CutRight's legacy format and is read as 1.0.0).
    /// Refuses an invalid policy, an empty pairing part or a pairing listed twice.
    pub fn from_json(json: &str) -> Result<Self, LadderError> {
        let raw: serde_json::Value = serde_json::from_str(json)?;
        let schema = raw.get("schema").and_then(|v| v.as_str());
        let version = raw.get("version").and_then(|v| v.as_str());
        match (schema, version) {
            (None, None) => {}
            (Some(schema), Some(version)) => {
                let parts: Vec<u64> = version
                    .split('.')
                    .filter_map(|part| part.parse().ok())
                    .collect();
                if schema != LADDER_SCHEMA || parts.len() != 3 || parts[0] != 1 || parts[1] > 0 {
                    return Err(LadderError::UnsupportedSnapshot {
                        schema: schema.to_owned(),
                        version: version.to_owned(),
                    });
                }
            }
            _ => {
                return Err(LadderError::UnsupportedSnapshot {
                    schema: schema.unwrap_or_default().to_owned(),
                    version: version.unwrap_or_default().to_owned(),
                })
            }
        }
        let snapshot: Snapshot = serde_json::from_value(raw)?;
        validate(&snapshot.policy)?;
        let mut states = BTreeMap::new();
        for pairing in snapshot.pairings {
            let key = LadderKey::new(&pairing.content_type, &pairing.platform)?;
            let state = LadderState {
                level: pairing.level,
                streak: pairing.streak,
            };
            if states.insert(key.clone(), state).is_some() {
                return Err(LadderError::DuplicateKey(format!(
                    "{}/{}",
                    key.content_type, key.platform
                )));
            }
        }
        Ok(Self {
            policy: snapshot.policy,
            states,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    policy: LadderPolicy,
    pairings: Vec<Pairing>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pairing {
    content_type: String,
    platform: String,
    level: Level,
    streak: u32,
}

fn validate(policy: &LadderPolicy) -> Result<(), LadderError> {
    if policy.approvals_to_review_light == 0 {
        return Err(LadderError::InvalidPolicy("approvals_to_review_light"));
    }
    if policy.approvals_to_autonomous == 0 {
        return Err(LadderError::InvalidPolicy("approvals_to_autonomous"));
    }
    Ok(())
}

fn apply(policy: &LadderPolicy, state: LadderState, result: Final) -> LadderState {
    match result {
        Final::Rejected => LadderState {
            level: Level::Reviewed,
            streak: 0,
        },
        Final::Approved {
            critic_failed: true,
        } => LadderState {
            level: match state.level {
                Level::Autonomous => Level::ReviewLight,
                other => other,
            },
            streak: 0,
        },
        Final::Approved {
            critic_failed: false,
        } => {
            let streak = state.streak.saturating_add(1);
            match state.level {
                Level::Reviewed if streak >= policy.approvals_to_review_light => LadderState {
                    level: Level::ReviewLight,
                    streak: 0,
                },
                Level::ReviewLight if streak >= policy.approvals_to_autonomous => LadderState {
                    level: Level::Autonomous,
                    streak: 0,
                },
                Level::Autonomous => LadderState {
                    level: Level::Autonomous,
                    streak: 0,
                },
                level => LadderState { level, streak },
            }
        }
    }
}
