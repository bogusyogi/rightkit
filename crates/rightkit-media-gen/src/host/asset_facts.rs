//! Three separate asset facts (PENDING 91) and the authoritative scope ids (PENDING 92).
//!
//! Plan §4 : every asset carries origin
//! (`generated`, `captured`, `acquired`, `supplied`), evidentiary meaning (`authentic` or
//! `illustrative`, independent of origin) and rights (asset-specific licence evidence,
//! attribution text, permitted uses, voice or likeness consent references), carried through every
//! derived asset. The plan fixes the origin and evidence values; every enum here adds `unknown`
//! (what old rows read as, never a guess) and rights adds a closed `status`
//! (`unknown`, `unresolved`, `cleared`) because the plan names rights fields but no status values.
//! Scope names and shape are the content-brief `channel` object: `{venture, channel, account}`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How the asset came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    #[default]
    Unknown,
    Generated,
    Captured,
    Acquired,
    Supplied,
}

/// What the asset is evidence of. Independent of origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    #[default]
    Unknown,
    Authentic,
    Illustrative,
}

/// Rights status. `Unknown` is an old row; `Unresolved` is a known gap that blocks delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightsStatus {
    #[default]
    Unknown,
    Unresolved,
    Cleared,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Origin {
    pub kind: OriginKind,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub job_id: Option<String>,
    /// Where an acquired or supplied asset came from (URL, path or supplier).
    pub source: Option<String>,
    /// Parent asset ids of a derived asset.
    pub parents: Vec<String>,
    /// The provider's own id for an acquired asset (Pexels photo id, NASA nasa_id, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// SHA-256 of the stored bytes: the dedup key for acquired and imported assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Rights {
    pub status: RightsStatus,
    /// Licence name as the provider states it (for example "CC BY 4.0", "Pexels License").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
    /// Licence URL as the provider states it, when it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence_url: Option<String>,
    pub licence_evidence: Vec<String>,
    pub source: Option<String>,
    pub attribution: Option<String>,
    pub permitted_uses: Vec<String>,
    pub consent_refs: Vec<String>,
}

/// The three facts, never merged into one status.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AssetFacts {
    pub origin: Origin,
    pub evidence: EvidenceKind,
    pub rights: Rights,
}

impl AssetFacts {
    /// What an asset recorded before these facts existed reads as.
    pub fn unknown() -> Self {
        Self::default()
    }

    /// A provider- or worker-generated asset. `evidence` is the caller's decision, normally
    /// [`job_evidence`] for the job kind and its parents: a synthesising kind is `illustrative`,
    /// a transforming kind inherits its parents' evidence, and nothing is `authentic` without
    /// authentic parents. Rights are inherited from `parents` when the asset is derived; a root
    /// generated asset has `unresolved` rights (nothing establishes any).
    pub fn generated(
        provider: &str,
        model: &str,
        job_id: &str,
        parents: Vec<String>,
        parent_rights: &[Rights],
        evidence: EvidenceKind,
    ) -> Self {
        let rights = if parent_rights.is_empty() {
            Rights {
                status: RightsStatus::Unresolved,
                ..Rights::default()
            }
        } else {
            Rights::inherit(parent_rights)
        };
        Self {
            origin: Origin {
                kind: OriginKind::Generated,
                provider: Some(provider.to_string()),
                model: Some(model.to_string()),
                job_id: Some(job_id.to_string()),
                source: None,
                parents,
                provider_id: None,
                content_sha256: None,
            },
            evidence,
            rights,
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Read a stored value; anything absent or unreadable is explicit `unknown`.
    pub fn from_value(v: &Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }
}

impl Rights {
    /// Rights of a derived asset: cleared only when every parent is cleared; the most
    /// restrictive status otherwise. Licence evidence, consent refs and attributions accumulate;
    /// permitted uses are the intersection (a derived asset may not be used wider than any parent).
    pub fn inherit(parents: &[Rights]) -> Rights {
        let status = if parents.iter().any(|p| p.status == RightsStatus::Unknown) {
            RightsStatus::Unknown
        } else if parents.iter().any(|p| p.status == RightsStatus::Unresolved) {
            RightsStatus::Unresolved
        } else {
            RightsStatus::Cleared
        };
        let mut out = Rights {
            status,
            ..Rights::default()
        };
        for p in parents {
            for e in &p.licence_evidence {
                if !out.licence_evidence.contains(e) {
                    out.licence_evidence.push(e.clone());
                }
            }
            for c in &p.consent_refs {
                if !out.consent_refs.contains(c) {
                    out.consent_refs.push(c.clone());
                }
            }
        }
        let attributions: Vec<&str> = parents
            .iter()
            .filter_map(|p| p.attribution.as_deref())
            .collect();
        if !attributions.is_empty() {
            let mut unique: Vec<&str> = Vec::new();
            for a in attributions {
                if !unique.contains(&a) {
                    unique.push(a);
                }
            }
            out.attribution = Some(unique.join("; "));
        }
        let mut uses = parents
            .first()
            .map(|p| p.permitted_uses.clone())
            .unwrap_or_default();
        for p in parents.iter().skip(1) {
            uses.retain(|u| p.permitted_uses.contains(u));
        }
        out.permitted_uses = uses;
        out
    }
}

/// Engine job kinds that synthesise new content (`cli_media::MODALITIES` less the transforming
/// kinds). Their output is `illustrative` even when the input was authentic: the model generated it.
/// Lipsync and the restyle and replace kinds are here because they generate new frames or audio.
const SYNTHESISING_KINDS: &[&str] = &[
    "image",
    "video",
    "voice",
    "avatar",
    "music",
    "sfx",
    "text",
    "lipsync",
    "extend",
    "motion_control",
    "replace_object",
    "restyle",
];

/// Engine job kinds that only transform existing media without synthesising new content. Their
/// output is as good as its inputs: `upscale` enlarges the parent pixels (the local Real-ESRGAN
/// route runs on the given file and generates no new scene).
const TRANSFORMING_KINDS: &[&str] = &["upscale", "transcribe"];

/// Evidentiary meaning of an asset a provider job produced, from the job kind and the evidence of
/// its parents (the library assets the request names as inputs).
///
/// Conservative rule, never a guess:
/// - a synthesising kind is `illustrative`, whatever its parents;
/// - a transforming kind is `authentic` only when every parent is `authentic`, `illustrative` when
///   any parent is `illustrative`, and otherwise `unknown` (a mixed or unrecorded parent set);
/// - a transforming kind with no parents is `unknown`: nothing establishes that the input was
///   authentic, so it is never `authentic` by omission;
/// - a kind this table does not name is `unknown`, not guessed as either meaning.
pub fn job_evidence(kind: &str, parent_evidence: &[EvidenceKind]) -> EvidenceKind {
    if SYNTHESISING_KINDS.contains(&kind) {
        return EvidenceKind::Illustrative;
    }
    if !TRANSFORMING_KINDS.contains(&kind) {
        return EvidenceKind::Unknown;
    }
    if parent_evidence.is_empty() {
        EvidenceKind::Unknown
    } else if parent_evidence
        .iter()
        .all(|e| *e == EvidenceKind::Authentic)
    {
        EvidenceKind::Authentic
    } else if parent_evidence.contains(&EvidenceKind::Illustrative) {
        EvidenceKind::Illustrative
    } else {
        EvidenceKind::Unknown
    }
}

/// Authoritative scope ids: the content-brief `channel` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub venture: String,
    pub channel: String,
    pub account: String,
}

/// `^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$`, the content-brief `id` pattern.
fn valid_id(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    s.len() <= 128
        && first.is_ascii_alphanumeric()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

impl Scope {
    /// Read the optional `venture`, `channel` and `account` keys of a request object. None given
    /// is `Ok(None)`; a partial or malformed scope is refused by name, never completed or guessed.
    pub fn from_request(input: &Value) -> Result<Option<Scope>, String> {
        let get = |k: &str| input.get(k).filter(|v| !v.is_null());
        let given: Vec<&str> = ["venture", "channel", "account"]
            .into_iter()
            .filter(|k| get(k).is_some())
            .collect();
        if given.is_empty() {
            return Ok(None);
        }
        let mut parts = Vec::new();
        for key in ["venture", "channel", "account"] {
            let Some(v) = get(key) else {
                return Err(format!(
                    "scope needs venture, channel and account together; missing {key}"
                ));
            };
            let Some(s) = v.as_str() else {
                return Err(format!("scope {key} must be a string id"));
            };
            if !valid_id(s) {
                return Err(format!(
                    "scope {key} must match ^[A-Za-z0-9][A-Za-z0-9_.:-]{{0,127}}$"
                ));
            }
            parts.push(s.to_string());
        }
        let account = parts.pop().unwrap_or_default();
        let channel = parts.pop().unwrap_or_default();
        let venture = parts.pop().unwrap_or_default();
        Ok(Some(Scope {
            venture,
            channel,
            account,
        }))
    }

    pub fn to_value(&self) -> Value {
        json!({"venture": self.venture, "channel": self.channel, "account": self.account})
    }
}
