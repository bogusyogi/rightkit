//! Blueprint doctor diagnostics: the typed `ready|degraded|broken|corrupt|missing` state
//! ladder and its reason codes, read from the native store at `.agent/graph/graph.db`
//! (open + `load_generation`). No `map.json` or `stale.json` is read; nothing writes them.

use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct DoctorReason {
    pub code: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DoctorDiagnostics {
    pub schema_version: u32,
    pub state: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub reasons: Vec<DoctorReason>,
}

fn blocked(state: &str, error: String, code: &str, message: &str) -> DoctorDiagnostics {
    DoctorDiagnostics {
        schema_version: 1,
        state: state.to_string(),
        errors: vec![error],
        warnings: vec![],
        reasons: vec![DoctorReason {
            code: code.to_string(),
            severity: "blocker".to_string(),
            message: message.to_string(),
        }],
    }
}

/// Native store readiness: `missing` when the store or its generation is absent, `corrupt`
/// when it cannot be opened or read, `degraded` otherwise.
pub fn collect_store_diagnostics(db_path: &Path) -> DoctorDiagnostics {
    if !db_path.exists() {
        return blocked(
            "missing",
            "blueprint native store is not present; run build/refresh first".to_string(),
            "missing_map",
            "Blueprint native store is not present; planner cannot retrieve candidates.",
        );
    }
    match crate::store::open_store_read_only(db_path) {
        Ok(connection) => match crate::store::load_generation(&connection) {
            Ok(Some(_generation)) => DoctorDiagnostics {
                schema_version: 1,
                state: "degraded".to_string(),
                errors: vec![],
                warnings: vec![],
                reasons: vec![],
            },
            Ok(None) => blocked(
                "missing",
                "blueprint native store has no persisted generation; run build/refresh first"
                    .to_string(),
                "missing_map",
                "Blueprint native store has no persisted generation; planner cannot retrieve candidates.",
            ),
            Err(error) => blocked(
                "corrupt",
                error.to_string(),
                "corrupt_map",
                "Blueprint native store could not be read.",
            ),
        },
        Err(error) => blocked(
            "corrupt",
            error.to_string(),
            "corrupt_map",
            "Blueprint native store could not be opened.",
        ),
    }
}
