//! Typed persona contracts and role-keyed lookup (FM-088, FM-104).
//!
//! The existing agent registry bakes exactly one fixed system prompt per
//! agent at build time (registry.rs::with_prompt) -- a persona in name
//! only, with no separate identity, versioning, or role indirection. This
//! module adds a typed contract and a registry that resolves a role to a
//! persona, without changing how agents are actually dispatched: nothing
//! here is wired into the live orchestrator by default. A caller opts in by
//! constructing a PersonaRegistry and consulting it; the existing
//! single-baked-in-prompt path is unaffected until something does.

use std::collections::BTreeMap;

/// A typed persona: an identity and a system prompt, indexed by the role it
/// applies to. `allowed_tools`, when set, narrows the tool surface available
/// under this persona; `None` means "defer to whatever ceiling the caller
/// already enforces" -- this type does not invent a new enforcement point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaContract {
    pub id: String,
    pub role: String,
    pub system_prompt: String,
    pub allowed_tools: Option<Vec<String>>,
    pub version: u32,
}

/// Reasons a persona contract cannot be registered or constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonaError {
    EmptyId,
    EmptyRole,
    EmptyPrompt,
    ZeroVersion,
    InvalidRolePackToml,
    InvalidRolePackField(&'static str),
    /// A persona for this role is already registered at this or a newer
    /// version. Registration never silently downgrades a role.
    NotNewerVersion {
        role: String,
        current: u32,
        attempted: u32,
    },
}

impl std::fmt::Display for PersonaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyId => write!(f, "persona id must not be empty"),
            Self::EmptyRole => write!(f, "persona role must not be empty"),
            Self::EmptyPrompt => write!(f, "persona system prompt must not be empty"),
            Self::ZeroVersion => write!(f, "persona version must be >= 1"),
            Self::InvalidRolePackToml => write!(f, "role pack is not valid TOML"),
            Self::InvalidRolePackField(field) => {
                write!(f, "role pack field {field:?} is missing or invalid")
            }
            Self::NotNewerVersion {
                role,
                current,
                attempted,
            } => write!(
                f,
                "role {role:?} already has persona version {current}; refusing downgrade to {attempted}"
            ),
        }
    }
}

impl PersonaContract {
    pub fn new(
        id: impl Into<String>,
        role: impl Into<String>,
        system_prompt: impl Into<String>,
    ) -> Result<Self, PersonaError> {
        let id = id.into();
        let role = role.into();
        let system_prompt = system_prompt.into();
        if id.trim().is_empty() {
            return Err(PersonaError::EmptyId);
        }
        if role.trim().is_empty() {
            return Err(PersonaError::EmptyRole);
        }
        if system_prompt.trim().is_empty() {
            return Err(PersonaError::EmptyPrompt);
        }
        Ok(Self {
            id,
            role,
            system_prompt,
            allowed_tools: None,
            version: 1,
        })
    }

    pub fn with_allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = Some(tools);
        self
    }

    pub fn with_version(mut self, version: u32) -> Result<Self, PersonaError> {
        if version == 0 {
            return Err(PersonaError::ZeroVersion);
        }
        self.version = version;
        Ok(self)
    }
}

/// Role-keyed persona lookup. One persona per role at any time; registering
/// a persona for a role that already has one requires a strictly newer
/// version, so a stale or accidental re-registration can never mask a
/// deliberate update -- and a deliberate rollback must go through
/// `replace`, not `register`, so it is visible in a caller's diff.
#[derive(Debug, Default)]
pub struct PersonaRegistry {
    by_role: BTreeMap<String, PersonaContract>,
}

impl PersonaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a persona for its role. Fails if a persona already exists
    /// for that role at the same or a newer version.
    pub fn register(&mut self, persona: PersonaContract) -> Result<(), PersonaError> {
        if let Some(existing) = self.by_role.get(&persona.role) {
            if persona.version <= existing.version {
                return Err(PersonaError::NotNewerVersion {
                    role: persona.role,
                    current: existing.version,
                    attempted: persona.version,
                });
            }
        }
        self.by_role.insert(persona.role.clone(), persona);
        Ok(())
    }

    /// Unconditionally replace whatever persona is registered for this role,
    /// including a downgrade. Use for explicit rollback, not routine updates.
    pub fn replace(&mut self, persona: PersonaContract) {
        self.by_role.insert(persona.role.clone(), persona);
    }

    /// Resolve the persona currently registered for a role, if any.
    pub fn persona_for_role(&self, role: &str) -> Option<&PersonaContract> {
        self.by_role.get(role)
    }

    pub fn roles(&self) -> Vec<&str> {
        self.by_role.keys().map(String::as_str).collect()
    }

    pub fn len(&self) -> usize {
        self.by_role.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_role.is_empty()
    }

    /// Build a registry from the three built-in role-pack fixtures. The
    /// fixture remains the source of truth for role, version, prompt, tools,
    /// permissions, and isolation; this method only adapts that document into
    /// typed persona contracts.
    pub fn from_builtin_role_packs() -> Result<Self, PersonaError> {
        let mut registry = Self::new();
        registry.register_builtin_role_packs()?;
        Ok(registry)
    }

    /// Populate this registry from the built-in role-pack fixtures.
    pub fn register_builtin_role_packs(&mut self) -> Result<(), PersonaError> {
        for raw in [
            include_str!("../../../fixtures/role-packs/coder.v1.toml"),
            include_str!("../../../fixtures/role-packs/orchestrator.v1.toml"),
            include_str!("../../../fixtures/role-packs/verifier.v1.toml"),
        ] {
            self.register_role_pack(raw)?;
        }
        Ok(())
    }

    /// Parse one role-pack document and register the persona it declares.
    /// Permissions and isolation are intentionally retained in the fixture's
    /// system prompt rather than invented as a second policy surface; the
    /// role-pack toolset becomes the persona's optional tool ceiling.
    pub fn register_role_pack(&mut self, raw: &str) -> Result<(), PersonaError> {
        let table: toml::Table = raw.parse().map_err(|_| PersonaError::InvalidRolePackToml)?;
        let schema_version = role_pack_u32(&table, "schema_version")?;
        if schema_version != 1 {
            return Err(PersonaError::InvalidRolePackField("schema_version"));
        }
        let role = role_pack_string(&table, "role")?;
        let system_prompt = role_pack_string(&table, "system_prompt")?;
        let toolset = role_pack_array(&table, "toolset")?;
        // These fields are required inputs to the persona prose even though
        // PersonaContract does not duplicate them as independent policy data.
        let _permissions = role_pack_array(&table, "permissions")?;
        let _isolation = role_pack_string(&table, "isolation")?;
        let persona =
            PersonaContract::new(format!("{role}.v{schema_version}"), role, system_prompt)?
                .with_allowed_tools(toolset)
                .with_version(schema_version)?;
        self.register(persona)
    }
}

fn role_pack_string(table: &toml::Table, key: &'static str) -> Result<String, PersonaError> {
    table
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(PersonaError::InvalidRolePackField(key))
}

fn role_pack_array(table: &toml::Table, key: &'static str) -> Result<Vec<String>, PersonaError> {
    let values = table
        .get(key)
        .and_then(|value| value.as_array())
        .ok_or(PersonaError::InvalidRolePackField(key))?;
    let values = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or(PersonaError::InvalidRolePackField(key))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.is_empty() {
        Err(PersonaError::InvalidRolePackField(key))
    } else {
        Ok(values)
    }
}

fn role_pack_u32(table: &toml::Table, key: &'static str) -> Result<u32, PersonaError> {
    table
        .get(key)
        .and_then(|value| value.as_integer())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or(PersonaError::InvalidRolePackField(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_fields() {
        assert_eq!(
            PersonaContract::new("", "reviewer", "prompt").unwrap_err(),
            PersonaError::EmptyId
        );
        assert_eq!(
            PersonaContract::new("p1", "", "prompt").unwrap_err(),
            PersonaError::EmptyRole
        );
        assert_eq!(
            PersonaContract::new("p1", "reviewer", "").unwrap_err(),
            PersonaError::EmptyPrompt
        );
    }

    #[test]
    fn with_version_rejects_zero() {
        let p = PersonaContract::new("p1", "reviewer", "prompt").unwrap();
        assert_eq!(p.with_version(0).unwrap_err(), PersonaError::ZeroVersion);
    }

    #[test]
    fn new_persona_defaults_to_version_one_with_no_tool_restriction() {
        let p = PersonaContract::new("p1", "reviewer", "prompt").unwrap();
        assert_eq!(p.version, 1);
        assert_eq!(p.allowed_tools, None);
    }

    #[test]
    fn register_resolves_by_role() {
        let mut registry = PersonaRegistry::new();
        let p = PersonaContract::new("p1", "reviewer", "review carefully").unwrap();
        registry.register(p.clone()).unwrap();
        assert_eq!(registry.persona_for_role("reviewer"), Some(&p));
        assert_eq!(registry.persona_for_role("unknown-role"), None);
    }

    #[test]
    fn register_refuses_same_or_older_version_for_an_occupied_role() {
        let mut registry = PersonaRegistry::new();
        let v2 = PersonaContract::new("p1", "reviewer", "v2 prompt")
            .unwrap()
            .with_version(2)
            .unwrap();
        registry.register(v2).unwrap();

        let v1_retry = PersonaContract::new("p1", "reviewer", "v1 prompt").unwrap(); // version 1
        let err = registry.register(v1_retry).unwrap_err();
        assert_eq!(
            err,
            PersonaError::NotNewerVersion {
                role: "reviewer".into(),
                current: 2,
                attempted: 1,
            }
        );
        // Registration failure must not have touched the existing entry.
        assert_eq!(registry.persona_for_role("reviewer").unwrap().version, 2);

        let v2_again = PersonaContract::new("p1", "reviewer", "same version")
            .unwrap()
            .with_version(2)
            .unwrap();
        assert!(
            registry.register(v2_again).is_err(),
            "equal version must also be refused"
        );
    }

    #[test]
    fn register_accepts_a_strictly_newer_version() {
        let mut registry = PersonaRegistry::new();
        registry
            .register(PersonaContract::new("p1", "reviewer", "v1").unwrap())
            .unwrap();
        let v2 = PersonaContract::new("p2", "reviewer", "v2")
            .unwrap()
            .with_version(2)
            .unwrap();
        registry.register(v2).unwrap();
        assert_eq!(registry.persona_for_role("reviewer").unwrap().id, "p2");
    }

    #[test]
    fn replace_allows_explicit_downgrade() {
        let mut registry = PersonaRegistry::new();
        registry
            .register(
                PersonaContract::new("p2", "reviewer", "v2")
                    .unwrap()
                    .with_version(2)
                    .unwrap(),
            )
            .unwrap();
        let rollback = PersonaContract::new("p1", "reviewer", "rollback").unwrap(); // version 1
        registry.replace(rollback);
        assert_eq!(registry.persona_for_role("reviewer").unwrap().version, 1);
    }

    #[test]
    fn distinct_roles_do_not_interfere() {
        let mut registry = PersonaRegistry::new();
        registry
            .register(PersonaContract::new("p1", "reviewer", "review").unwrap())
            .unwrap();
        registry
            .register(PersonaContract::new("p2", "planner", "plan").unwrap())
            .unwrap();
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.persona_for_role("reviewer").unwrap().id, "p1");
        assert_eq!(registry.persona_for_role("planner").unwrap().id, "p2");
    }

    #[test]
    fn allowed_tools_narrows_without_being_required() {
        let unrestricted = PersonaContract::new("p1", "reviewer", "prompt").unwrap();
        assert_eq!(unrestricted.allowed_tools, None);

        let restricted = PersonaContract::new("p2", "reviewer", "prompt")
            .unwrap()
            .with_allowed_tools(vec!["read_file".into(), "grep".into()]);
        assert_eq!(
            restricted.allowed_tools,
            Some(vec!["read_file".into(), "grep".into()])
        );
    }

    #[test]
    fn empty_registry_reports_empty_and_zero_length() {
        let registry = PersonaRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.roles().is_empty());
    }

    #[test]
    fn builtin_role_packs_populate_typed_personas() {
        let registry = PersonaRegistry::from_builtin_role_packs().unwrap();
        assert_eq!(registry.len(), 3);

        let coder = registry.persona_for_role("coder").unwrap();
        assert_eq!(coder.id, "coder.v1");
        assert_eq!(coder.version, 1);
        assert_eq!(
            coder.allowed_tools.as_ref().unwrap(),
            &vec!["coding".to_string()]
        );
        assert!(coder.system_prompt.contains("coding"));
        assert!(coder.system_prompt.contains("workspace-edit and test"));
        assert!(coder.system_prompt.contains("fresh-task"));

        let orchestrator = registry.persona_for_role("orchestrator").unwrap();
        assert_eq!(orchestrator.id, "orchestrator.v1");
        assert_eq!(
            orchestrator.allowed_tools.as_ref().unwrap(),
            &vec!["orchestration-readonly".to_string()]
        );
        assert!(orchestrator.system_prompt.contains("supervise"));

        let verifier = registry.persona_for_role("verifier").unwrap();
        assert_eq!(verifier.id, "verifier.v1");
        assert_eq!(
            verifier.allowed_tools.as_ref().unwrap(),
            &vec!["verification-readonly".to_string()]
        );
        assert!(verifier.system_prompt.contains("inspect and run-checks"));
        assert!(verifier.system_prompt.contains("producer-independent"));
    }

    #[test]
    fn role_pack_registration_requires_prompt_and_policy_fields() {
        let raw = r#"
            schema_version = 1
            role = "reviewer"
            system_prompt = "Review with the declared tools."
            toolset = ["Read"]
            permissions = ["inspect"]
            isolation = "producer-independent"
        "#;
        let mut registry = PersonaRegistry::new();
        registry.register_role_pack(raw).unwrap();
        let persona = registry.persona_for_role("reviewer").unwrap();
        assert_eq!(persona.id, "reviewer.v1");
        assert_eq!(persona.system_prompt, "Review with the declared tools.");
        assert_eq!(persona.allowed_tools, Some(vec!["Read".into()]));

        let missing_prompt = raw.replace(
            "system_prompt = \"Review with the declared tools.\"",
            "# system_prompt intentionally absent",
        );
        assert_eq!(
            registry.register_role_pack(&missing_prompt).unwrap_err(),
            PersonaError::InvalidRolePackField("system_prompt")
        );
    }

    #[test]
    fn builtin_registration_rejects_invalid_toml() {
        let mut registry = PersonaRegistry::new();
        assert_eq!(
            registry.register_role_pack("not = [valid").unwrap_err(),
            PersonaError::InvalidRolePackToml
        );
    }
}
