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
            include_str!("../../data/coding/role-packs/coder.v1.toml"),
            include_str!("../../data/coding/role-packs/orchestrator.v1.toml"),
            include_str!("../../data/coding/role-packs/verifier.v1.toml"),
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
