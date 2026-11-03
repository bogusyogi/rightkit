//! Content preferences shared by streaming and completed Markdown renderers.
//!
//! Seed before startup previews, then refresh when resolved session settings become active.
//! Tests isolate preferences per thread so parallel renderer tests cannot affect one another.
//!
//! Adapted seam: the donor stores `codex_config::types::TuiRendering`;
//! the host app does not absorb the config crate, so the donor's option record is
//! reproduced here verbatim (all renderers on by default) and `init` is the
//! parent-owned config cutover point.

/// Optional rich renderers. Disabled renderers preserve the original source.
/// (Donor `codex_config::types::TuiRendering`, serde/schemars dropped.)
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TuiRendering {
    /// Render Mermaid code blocks as diagrams.
    pub mermaid: bool,
    /// Render math expressions using Unicode notation.
    pub math: bool,
    /// Render pipe tables, including tables inside Markdown fences.
    pub tables: bool,
    /// Render Markdown bullets and task-list markers as Unicode symbols.
    pub lists: bool,
}

impl Default for TuiRendering {
    fn default() -> Self {
        Self {
            mermaid: true,
            math: true,
            tables: true,
            lists: true,
        }
    }
}

static RENDERING: std::sync::LazyLock<std::sync::RwLock<TuiRendering>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(TuiRendering::default()));

pub(crate) fn current() -> TuiRendering {
    {
        *RENDERING
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
