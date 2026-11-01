//! Pure, versioned role-pack resolution against a caller-supplied live catalog.
//!
//! This module deliberately returns assignment data only.  It never evaluates
//! an action or changes a previously locked permission decision.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveModel {
    pub id: String,
    pub context_window_tokens: u64,
    pub cheap: bool,
    pub provider_family: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RolePack {
    pub schema_version: u32,
    pub role: String,
    pub primary: String,
    pub cheap_candidate: String,
    pub fallbacks: Vec<String>,
    pub verifier: String,
    pub reasoning: String,
    pub context_budget: u64,
    pub toolset: Vec<String>,
    pub permissions: Vec<String>,
    pub isolation: String,
    pub max_turns: u32,
    pub max_cost_usd: f64,
    pub retry_limit: u8,
    pub escalate_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierBinding {
    pub model_id: String,
    pub independent: bool,
    pub degradation_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRolePack {
    pub pack: RolePack,
    pub primary: LiveModel,
    pub cheap_candidate: LiveModel,
    pub fallbacks: Vec<LiveModel>,
    pub verifier: LiveModel,
    pub verifier_binding: VerifierBinding,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RolePackResolutionError {
    #[error("role pack field `{0}` is missing or invalid")]
    InvalidField(&'static str),
    #[error("role pack references unknown model `{0}`")]
    UnknownModel(String),
    #[error("role pack model `{model}` is unqualified: {reason}")]
    UnqualifiedModel { model: String, reason: String },
}

pub fn resolve_role_pack(
    raw: &str,
    live_catalog: &[LiveModel],
) -> Result<ResolvedRolePack, RolePackResolutionError> {
    let pack = parse_role_pack(raw)?;
    if pack.schema_version != 1 {
        return Err(RolePackResolutionError::InvalidField("schema_version"));
    }
    if pack.role.trim().is_empty()
        || pack.reasoning.trim().is_empty()
        || pack.toolset.is_empty()
        || pack.permissions.is_empty()
        || pack.isolation.trim().is_empty()
        || pack.context_budget == 0
        || pack.max_turns == 0
        || pack.escalate_on.is_empty()
    {
        return Err(RolePackResolutionError::InvalidField(
            "required v1 assignment",
        ));
    }

    let lookup = |id: &str| {
        live_catalog
            .iter()
            .find(|model| model.id == id)
            .cloned()
            .ok_or_else(|| RolePackResolutionError::UnknownModel(id.to_string()))
    };
    let primary = lookup(&pack.primary)?;
    let cheap_candidate = lookup(&pack.cheap_candidate)?;
    let fallbacks = pack
        .fallbacks
        .iter()
        .map(|id| lookup(id))
        .collect::<Result<Vec<_>, _>>()?;
    let verifier = lookup(&pack.verifier)?;
    for model in [&primary, &cheap_candidate, &verifier]
        .into_iter()
        .chain(fallbacks.iter())
    {
        if model.context_window_tokens < pack.context_budget {
            return Err(RolePackResolutionError::UnqualifiedModel {
                model: model.id.clone(),
                reason: format!(
                    "context window {} is below role budget {}",
                    model.context_window_tokens, pack.context_budget
                ),
            });
        }
    }
    if !cheap_candidate.cheap {
        return Err(RolePackResolutionError::UnqualifiedModel {
            model: cheap_candidate.id.clone(),
            reason: "cheap_candidate is not catalogued as cheap".into(),
        });
    }
    let independent =
        primary.id != verifier.id && primary.provider_family != verifier.provider_family;
    Ok(ResolvedRolePack {
        verifier_binding: VerifierBinding {
            model_id: verifier.id.clone(),
            independent,
            degradation_reason: (!independent)
                .then(|| "role-pack verifier is not from a different provider family".to_string()),
        },
        pack,
        primary,
        cheap_candidate,
        fallbacks,
        verifier,
    })
}

fn parse_role_pack(raw: &str) -> Result<RolePack, RolePackResolutionError> {
    let table: toml::Table = raw
        .parse()
        .map_err(|_| RolePackResolutionError::InvalidField("<toml>"))?;
    Ok(RolePack {
        schema_version: parse_u32(&table, "schema_version")?,
        role: parse_string(&table, "role")?,
        primary: parse_string(&table, "primary")?,
        cheap_candidate: parse_string(&table, "cheap_candidate")?,
        fallbacks: parse_array(&table, "fallbacks")?,
        verifier: parse_string(&table, "verifier")?,
        reasoning: parse_string(&table, "reasoning")?,
        context_budget: parse_u64(&table, "context_budget")?,
        toolset: parse_array(&table, "toolset")?,
        permissions: parse_array(&table, "permissions")?,
        isolation: parse_string(&table, "isolation")?,
        max_turns: parse_u32(&table, "max_turns")?,
        max_cost_usd: table
            .get("max_cost_usd")
            .and_then(|v| v.as_float())
            .filter(|v: &f64| v.is_finite() && *v > 0.0)
            .ok_or(RolePackResolutionError::InvalidField("max_cost_usd"))?,
        retry_limit: parse_u32(&table, "retry_limit")?
            .try_into()
            .map_err(|_| RolePackResolutionError::InvalidField("retry_limit"))?,
        escalate_on: parse_array(&table, "escalate_on")?,
    })
}

#[cfg(test)]
fn parse_line(line: &str) -> Option<(String, String)> {
    let mut quoted = false;
    let mut comment = None;
    for (i, c) in line.char_indices() {
        if c == '"' {
            quoted = !quoted;
        }
        if c == '#' && !quoted {
            comment = Some(i);
            break;
        }
    }
    let line = line[..comment.unwrap_or(line.len())].trim();
    let (key, value) = line.split_once('=')?;
    Some((key.trim().to_string(), value.trim().to_string()))
}

fn parse_string(table: &toml::Table, key: &'static str) -> Result<String, RolePackResolutionError> {
    table
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or(RolePackResolutionError::InvalidField(key))
}

fn parse_array(
    table: &toml::Table,
    key: &'static str,
) -> Result<Vec<String>, RolePackResolutionError> {
    let array = table
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or(RolePackResolutionError::InvalidField(key))?;
    let items = array
        .iter()
        .map(|v| v.as_str().map(str::trim).filter(|v| !v.is_empty()))
        .collect::<Option<Vec<_>>>()
        .ok_or(RolePackResolutionError::InvalidField(key))?
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if items.is_empty() {
        Err(RolePackResolutionError::InvalidField(key))
    } else {
        Ok(items)
    }
}

fn parse_u32(table: &toml::Table, key: &'static str) -> Result<u32, RolePackResolutionError> {
    table
        .get(key)
        .and_then(|v| v.as_integer())
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or(RolePackResolutionError::InvalidField(key))
}
fn parse_u64(table: &toml::Table, key: &'static str) -> Result<u64, RolePackResolutionError> {
    table
        .get(key)
        .and_then(|v| v.as_integer())
        .and_then(|v| u64::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or(RolePackResolutionError::InvalidField(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_raw() -> String {
        r#"
schema_version = 1
role = "reviewer"
primary = "model-strong"
cheap_candidate = "model-cheap"
fallbacks = ["model-fallback"]
verifier = "model-verifier"
reasoning = "high"
context_budget = 50000
toolset = ["Read", "Grep"]
permissions = ["read"]
isolation = "sandboxed"
max_turns = 10
max_cost_usd = 2.5
retry_limit = 2
escalate_on = ["timeout"]
"#
        .to_string()
    }

    fn catalog() -> Vec<LiveModel> {
        vec![
            LiveModel {
                id: "model-strong".into(),
                context_window_tokens: 200_000,
                cheap: false,
                provider_family: "anthropic".into(),
            },
            LiveModel {
                id: "model-cheap".into(),
                context_window_tokens: 100_000,
                cheap: true,
                provider_family: "anthropic".into(),
            },
            LiveModel {
                id: "model-fallback".into(),
                context_window_tokens: 100_000,
                cheap: false,
                provider_family: "anthropic".into(),
            },
            LiveModel {
                id: "model-verifier".into(),
                context_window_tokens: 100_000,
                cheap: false,
                provider_family: "openai".into(),
            },
        ]
    }

    #[test]
    fn resolves_valid_role_pack_against_live_catalog() {
        let resolved = resolve_role_pack(&valid_raw(), &catalog()).unwrap();
        assert_eq!(resolved.primary.id, "model-strong");
        assert_eq!(resolved.cheap_candidate.id, "model-cheap");
        assert_eq!(resolved.fallbacks.len(), 1);
        assert_eq!(resolved.fallbacks[0].id, "model-fallback");
        assert_eq!(resolved.verifier.id, "model-verifier");
        assert!(resolved.verifier_binding.independent);
        assert!(resolved.verifier_binding.degradation_reason.is_none());
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let raw = valid_raw().replace("schema_version = 1", "schema_version = 2");
        let error = resolve_role_pack(&raw, &catalog()).unwrap_err();
        assert_eq!(
            error,
            RolePackResolutionError::InvalidField("schema_version")
        );
    }

    #[test]
    fn rejects_missing_required_field() {
        let raw = valid_raw().replace("role = \"reviewer\"\n", "");
        let error = resolve_role_pack(&raw, &catalog()).unwrap_err();
        assert!(matches!(error, RolePackResolutionError::InvalidField(_)));
    }

    #[test]
    fn rejects_reference_to_unknown_model() {
        let raw = valid_raw().replace("model-verifier", "model-ghost");
        let error = resolve_role_pack(&raw, &catalog()).unwrap_err();
        assert_eq!(
            error,
            RolePackResolutionError::UnknownModel("model-ghost".to_string())
        );
    }

    #[test]
    fn rejects_model_with_context_window_below_role_budget() {
        let mut small_catalog = catalog();
        small_catalog[0].context_window_tokens = 1_000; // primary, below 50000 budget
        let error = resolve_role_pack(&valid_raw(), &small_catalog).unwrap_err();
        match error {
            RolePackResolutionError::UnqualifiedModel { model, reason } => {
                assert_eq!(model, "model-strong");
                assert!(reason.contains("context window"));
            }
            other => panic!("expected UnqualifiedModel, got {other:?}"),
        }
    }

    #[test]
    fn rejects_cheap_candidate_not_flagged_cheap_in_catalog() {
        let mut not_cheap_catalog = catalog();
        not_cheap_catalog[1].cheap = false; // model-cheap
        let error = resolve_role_pack(&valid_raw(), &not_cheap_catalog).unwrap_err();
        match error {
            RolePackResolutionError::UnqualifiedModel { model, reason } => {
                assert_eq!(model, "model-cheap");
                assert!(reason.contains("not catalogued as cheap"));
            }
            other => panic!("expected UnqualifiedModel, got {other:?}"),
        }
    }

    #[test]
    fn flags_degradation_when_verifier_shares_provider_family_with_primary() {
        let mut same_family_catalog = catalog();
        // verifier now shares provider_family with primary ("anthropic").
        same_family_catalog[3].provider_family = "anthropic".into();
        let resolved = resolve_role_pack(&valid_raw(), &same_family_catalog).unwrap();
        assert!(!resolved.verifier_binding.independent);
        assert_eq!(
            resolved.verifier_binding.degradation_reason.as_deref(),
            Some("role-pack verifier is not from a different provider family")
        );
    }

    #[test]
    fn parse_line_strips_unquoted_comments_but_preserves_hash_in_quotes() {
        assert_eq!(
            parse_line("key = \"value\" # a comment"),
            Some(("key".to_string(), "\"value\"".to_string()))
        );
        assert_eq!(
            parse_line("key = \"val#ue\""),
            Some(("key".to_string(), "\"val#ue\"".to_string()))
        );
        assert_eq!(parse_line("# just a comment"), None);
        assert_eq!(parse_line(""), None);
    }
}
