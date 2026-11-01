//! Freshness acquisition and request-bound freshness policy.
//!
//! The source is injected by composition.  Unknown, stale, or incoherent
//! observations never become current through a local fallback.

use crate::release::ReleaseBinding;
use rightkit_context_protocol::{FreshnessSnapshotV1, ReleaseGenerationStatus};
use rightkit_context_provider_sdk::source::{FreshnessSource, SourceQuery};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshnessRequirement {
    Any,
    Current,
    Exact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshnessState {
    Current,
    Stale,
    Unknown,
    GenerationMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshnessErrorCode {
    Unavailable,
    Malformed,
    GenerationMismatch,
    RequirementUnsatisfied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FreshnessError {
    Unavailable(String),
    Malformed(&'static str),
    GenerationMismatch,
    RequirementUnsatisfied,
}

impl FreshnessError {
    pub const fn code(&self) -> FreshnessErrorCode {
        match self {
            Self::Unavailable(_) => FreshnessErrorCode::Unavailable,
            Self::Malformed(_) => FreshnessErrorCode::Malformed,
            Self::GenerationMismatch => FreshnessErrorCode::GenerationMismatch,
            Self::RequirementUnsatisfied => FreshnessErrorCode::RequirementUnsatisfied,
        }
    }
}

impl fmt::Display for FreshnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail) => write!(f, "freshness_unavailable:{detail}"),
            Self::Malformed(detail) => write!(f, "freshness_malformed:{detail}"),
            Self::GenerationMismatch => f.write_str("freshness_generation_mismatch"),
            Self::RequirementUnsatisfied => f.write_str("freshness_requirement_unsatisfied"),
        }
    }
}

impl std::error::Error for FreshnessError {}

/// Freshness provenance bound before any provider content is acquired.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreshnessBinding {
    pub snapshot: FreshnessSnapshotV1,
    pub state: FreshnessState,
    pub source_id: Option<String>,
    pub provenance: Option<String>,
    pub release: Option<ReleaseBinding>,
}

impl FreshnessBinding {
    pub fn from_snapshot(
        snapshot: FreshnessSnapshotV1,
        release: Option<ReleaseBinding>,
    ) -> Result<Self, FreshnessError> {
        validate_snapshot(&snapshot)?;
        let mut state = if snapshot.stale {
            FreshnessState::Stale
        } else if snapshot.generation.as_deref().is_none() {
            FreshnessState::Unknown
        } else {
            FreshnessState::Current
        };
        if let Some(binding) = &release {
            if binding.status == ReleaseGenerationStatus::Mismatch {
                state = FreshnessState::GenerationMismatch;
            } else if binding.status != ReleaseGenerationStatus::Matched {
                state = FreshnessState::Unknown;
            }
            if let Some(observed) = snapshot.generation.as_deref() {
                if binding.expected_generation != observed {
                    state = FreshnessState::GenerationMismatch;
                }
            }
        }
        Ok(Self {
            snapshot,
            state,
            source_id: None,
            provenance: None,
            release,
        })
    }

    /// A freshness binding synthesized when the freshness source could not be
    /// consulted at all: the Hub/watcher is not ready, the source is missing,
    /// or it reported itself incomplete/unavailable. The planner degrades to
    /// this rather than failing the whole federation — providers then run
    /// against the release-generation fallback and the degradation is recorded
    /// on the response. It is NEVER used for a generation mismatch or a
    /// malformed snapshot: those remain fail-closed. State is `Unknown`
    /// (`warning_code()` == "freshness_unavailable"), never fabricated Current.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            snapshot: FreshnessSnapshotV1 {
                graph_state: "unavailable".to_owned(),
                generation: None,
                snapshot_id: None,
                base_commit: None,
                overlay_digest: None,
                stale: false,
            },
            state: FreshnessState::Unknown,
            source_id: None,
            provenance: Some(reason.into()),
            release: None,
        }
    }

    pub fn with_provenance(
        mut self,
        source_id: impl Into<String>,
        provenance: Option<String>,
    ) -> Self {
        self.source_id = Some(source_id.into());
        self.provenance = provenance;
        self
    }

    /// Acquire the central verdict through the injected SDK source.
    pub async fn acquire<S: FreshnessSource + ?Sized>(
        source: &S,
        query: &SourceQuery,
        release: Option<ReleaseBinding>,
    ) -> Result<Self, FreshnessError> {
        let response = source
            .freshness(query)
            .await
            .map_err(|error| FreshnessError::Unavailable(error.to_string()))?;
        if !response.complete {
            // The source states why it is incomplete -- it puts the verdict's
            // own first reason on a warning. Refusing with a bare
            // `source_incomplete` threw that away, so every distinct cause
            // (an unstable graph, a missing snapshot, an unreadable overlay)
            // reached the operator as one word.
            let detail = response
                .warnings
                .iter()
                .find_map(|warning| warning.detail_id.clone());
            return Err(FreshnessError::Unavailable(match detail {
                Some(detail) => format!("source_incomplete: {detail}"),
                None => "source_incomplete".to_owned(),
            }));
        }
        let mut binding = Self::from_snapshot(response.value, release)?;
        binding.provenance = response
            .warnings
            .first()
            .and_then(|warning| warning.detail_id.clone());
        Ok(binding)
    }

    pub fn require(
        &self,
        requirement: FreshnessRequirement,
        expected_generation: Option<&str>,
    ) -> Result<(), FreshnessError> {
        match requirement {
            FreshnessRequirement::Any => Ok(()),
            FreshnessRequirement::Current => self
                .is_current()
                .then_some(())
                .ok_or(FreshnessError::RequirementUnsatisfied),
            FreshnessRequirement::Exact => {
                if !self.is_current() {
                    return Err(FreshnessError::RequirementUnsatisfied);
                }
                let expected = expected_generation
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or(FreshnessError::Malformed(
                        "exact freshness requires generation",
                    ))?;
                (self.snapshot.generation.as_deref() == Some(expected))
                    .then_some(())
                    .ok_or(FreshnessError::GenerationMismatch)
            }
        }
    }

    pub fn is_current(&self) -> bool {
        self.state == FreshnessState::Current
    }

    pub fn warning_code(&self) -> Option<&'static str> {
        match self.state {
            FreshnessState::Current => None,
            FreshnessState::Stale => Some("freshness_stale"),
            FreshnessState::Unknown => Some("freshness_unavailable"),
            FreshnessState::GenerationMismatch => Some("generation_incoherent"),
        }
    }
}

/// A graph snapshot's generation is the provider's own content identity, not
/// a release generation.
///
/// This was validated with `release::validate_generation`, which requires the
/// release form `sha256:<64 hex>`. Blueprint content-addresses its generations
/// as `xxh128:<32 hex>`, so every snapshot it produced was rejected as
/// malformed and no installed Membrane could bind freshness at all. The
/// snapshot only needs an algorithm-tagged digest it can be compared by, so
/// that is what is required here — still strict enough to refuse an untagged
/// or non-hex value.
fn validate_snapshot_generation(generation: &str) -> Result<(), FreshnessError> {
    let Some((algorithm, digest)) = generation.split_once(':') else {
        return Err(FreshnessError::Malformed("generation is invalid"));
    };
    let algorithm_ok = (2..=16).contains(&algorithm.len())
        && algorithm
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    let digest_ok = (16..=128).contains(&digest.len())
        && digest.len() % 2 == 0
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    if algorithm_ok && digest_ok {
        Ok(())
    } else {
        Err(FreshnessError::Malformed("generation is invalid"))
    }
}

fn validate_snapshot(snapshot: &FreshnessSnapshotV1) -> Result<(), FreshnessError> {
    let graph_state = snapshot.graph_state.trim();
    if graph_state.is_empty() {
        return Err(FreshnessError::Malformed("graph_state is empty"));
    }
    if snapshot.stale && snapshot.generation.is_none() {
        return Ok(());
    }
    if let Some(generation) = snapshot.generation.as_deref() {
        validate_snapshot_generation(generation)?;
    }
    if snapshot.overlay_digest.is_some() && snapshot.base_commit.is_none() {
        return Err(FreshnessError::Malformed("overlay requires base_commit"));
    }
    Ok(())
}
