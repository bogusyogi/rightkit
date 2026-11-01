use crate::minimize::error::MinimizeError;
use crate::minimize::review::MinimizePaths;
use std::path::{Path, PathBuf};

/// Resolve the minimize policy asset (`lib/minimize/POLICY.md`) under the
/// first of `roots` that contains it. The host application passes its own
/// release or source roots. The validator is the running executable, so
/// changing the implementation invalidates earlier receipts.
pub fn resolve_minimize_paths(roots: &[PathBuf]) -> Result<MinimizePaths, MinimizeError> {
    let policy_name = Path::new("lib/minimize/POLICY.md");
    let validator = std::env::current_exe().map_err(|error| {
        MinimizeError::new(format!("native minimize validator unavailable: {error}"))
    })?;
    for root in roots {
        let policy = root.join(policy_name);
        if policy.is_file() {
            return Ok(MinimizePaths {
                policy_path: policy,
                validator_path: validator,
            });
        }
    }
    Err(MinimizeError::new(
        "minimize policy asset lib/minimize/POLICY.md is unavailable from the supplied roots",
    ))
}
