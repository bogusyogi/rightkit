//! Bounded, user-owned context sections shared with team peers.
//!
//! The context is deliberately local and synchronous: it provides a prompt
//! snapshot for a conductor, not a transport or a claim that another peer has
//! received it. No external artifact is required or represented here.

use std::collections::BTreeMap;

use thiserror::Error;

/// Maximum bytes in one shared section, so one write cannot monopolize the
/// prompt context.
pub const MAX_SECTION_BYTES: usize = 16 * 1024;
/// Maximum bytes across all stored section names, owners, and contents, so
/// the shared prompt remains bounded even when a team owns several sections.
pub const MAX_CONTEXT_BYTES: usize = 64 * 1024;
/// Maximum bytes in a section name, keeping the bounded context metadata
/// finite as well as its contents.
pub const MAX_SECTION_NAME_BYTES: usize = 256;
/// Maximum bytes in an owner id, so ownership metadata cannot evade bounds.
pub const MAX_OWNER_ID_BYTES: usize = 256;

/// A rejected section write, kept typed so callers cannot mistake a refused
/// ownership transfer or oversized context for a successful update.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SectionContextError {
    /// The section already belongs to another peer, so this write cannot
    /// silently replace that peer's context.
    #[error("section `{section}` is owned by peer `{owner}`, not `{requester}`")]
    OwnershipConflict {
        section: String,
        owner: String,
        requester: String,
    },
    /// The new section would exceed the per-section prompt bound.
    #[error("section content is {actual} bytes; maximum is {maximum}")]
    SectionTooLarge { actual: usize, maximum: usize },
    /// The new section would exceed the aggregate prompt bound.
    #[error("shared section context would be {actual} bytes; maximum is {maximum}")]
    ContextTooLarge { actual: usize, maximum: usize },
    /// Empty or oversized section names are rejected before they enter the
    /// shared map.
    #[error("section name must be non-empty and at most {maximum} bytes")]
    InvalidSectionName { maximum: usize },
    /// Empty or oversized owner ids are rejected so every write has explicit
    /// ownership.
    #[error("section owner must be non-empty and at most {maximum} bytes")]
    InvalidOwner { maximum: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Section {
    owner: String,
    content: String,
}

/// Synchronous shared context supplied by a trusted user-facing host. Peer
/// execution receives only immutable reads of this value; it never receives a
/// mutable `SectionContext` or a public writer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionContext {
    sections: BTreeMap<String, Section>,
    stored_bytes: usize,
}

fn stored_entry_bytes(section: &str, owner: &str, content: &str) -> usize {
    section
        .len()
        .saturating_add(owner.len())
        .saturating_add(content.len())
}

impl SectionContext {
    /// Create an empty context; absent sections remain ordinary `None` reads.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace one section from the user-owned configuration surface. The
    /// `user` owner is fixed by this API so agent and peer ids cannot claim a
    /// shared brief as their own.
    pub fn set_by_user(
        &mut self,
        section: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<(), SectionContextError> {
        self.write(section, "user", content)
    }

    /// Trusted in-crate setup path. This is intentionally not public: peers
    /// get read-only prompt snapshots through `TeamRuntime`.
    pub(crate) fn write(
        &mut self,
        section: impl Into<String>,
        owner: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<(), SectionContextError> {
        let section = section.into();
        let owner = owner.into();
        let content = content.into();

        if section.is_empty() || section.len() > MAX_SECTION_NAME_BYTES {
            return Err(SectionContextError::InvalidSectionName {
                maximum: MAX_SECTION_NAME_BYTES,
            });
        }
        if owner.is_empty() || owner.len() > MAX_OWNER_ID_BYTES {
            return Err(SectionContextError::InvalidOwner {
                maximum: MAX_OWNER_ID_BYTES,
            });
        }
        if content.len() > MAX_SECTION_BYTES {
            return Err(SectionContextError::SectionTooLarge {
                actual: content.len(),
                maximum: MAX_SECTION_BYTES,
            });
        }

        if let Some(existing) = self.sections.get(&section) {
            if existing.owner != owner {
                return Err(SectionContextError::OwnershipConflict {
                    section,
                    owner: existing.owner.clone(),
                    requester: owner,
                });
            }
        }

        let previous_bytes = self.sections.get(&section).map_or(0, |existing| {
            stored_entry_bytes(&section, &existing.owner, &existing.content)
        });
        let actual = self
            .stored_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(stored_entry_bytes(&section, &owner, &content));
        if actual > MAX_CONTEXT_BYTES {
            return Err(SectionContextError::ContextTooLarge {
                actual,
                maximum: MAX_CONTEXT_BYTES,
            });
        }

        self.stored_bytes = actual;
        self.sections.insert(section, Section { owner, content });
        Ok(())
    }

    /// Read a section without allocation or failure; a section not yet owned
    /// by the team is represented as `None` rather than an error.
    pub fn read(&self, section: &str) -> Option<&str> {
        self.sections
            .get(section)
            .map(|section| section.content.as_str())
    }

    /// Render the owned sections deterministically for conductor prompt
    /// injection, while keeping the ownership labels out of peer prompts.
    pub(crate) fn prompt_fragment(&self) -> Option<String> {
        if self.sections.is_empty() {
            return None;
        }

        let mut fragment = String::from("## Shared Team Context\n");
        for (name, section) in &self.sections {
            fragment.push_str("\n### ");
            fragment.push_str(name);
            fragment.push('\n');
            fragment.push_str(&section.content);
        }
        Some(fragment)
    }
}
