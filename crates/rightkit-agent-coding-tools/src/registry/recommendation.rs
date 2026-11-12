// Toolset-size recommendation: advisory warning when too many active tools
// are registered at session startup (par sect#204).
//
// The threshold (25) is chosen as a round number between the default full
// registry (~45-50 tools) and a reasonable curated subset. Goose's default
// extension-manager warning fires around the same level. Users who see the
// warning can narrow scope with `--toolset <profile>` or `--allowed-tools`.

/// Default active-tool count threshold for the size advisory.
pub const TOOLSET_SIZE_THRESHOLD: usize = 25;

/// A structured toolset-size warning.
#[derive(Debug, Clone)]
pub struct ToolsetWarning {
    pub active_count: usize,
    pub threshold: usize,
    pub message: String,
}

/// Returns `Some(ToolsetWarning)` when `active_count` exceeds the threshold.
/// The message is advisory-only — the run proceeds regardless.
pub fn toolset_size_warning(active_count: usize) -> Option<ToolsetWarning> {
    if active_count > TOOLSET_SIZE_THRESHOLD {
        Some(ToolsetWarning {
            active_count,
            threshold: TOOLSET_SIZE_THRESHOLD,
            message: format!(
                "[run] warning: {active_count} tools active (threshold {TOOLSET_SIZE_THRESHOLD}) — \
consider --toolset <profile> or --allowed-tools to narrow scope for better tool-selection accuracy"
            ),
        })
    } else {
        None
    }
}
