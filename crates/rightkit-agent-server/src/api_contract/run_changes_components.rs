// I1 — Authoritative review openapi components fragment.
//
// Standalone OpenAPI schema fragment for the run-scoped changes
// surface. `api_contract/components.rs` is the structural composition
// point the I2 integrator extends; this file keeps I1's schemas in
// one place so the integrator can merge them in one helper-call
// without editing any fragment definition.
//
// Schema list (14 entries) is the contract fragment for
// `engine/src/api_run_changes.rs`:
//   - ChangeEntry, ChangeClassification, ChangeStage, AnchorStatus,
//     CommentKind
//   - CaptureSnapshotRequest, CaptureSnapshotResponse
//   - InventoryRequest, RunChangesInventory, RunChangesSummary,
//     GitBaselineSummary
//   - UndoRequest, UndoResponse
//   - RevisionUpdateRequest, RevisionUpdateResponse
//   - DiffBodyRequest, DiffBodyResponse, DiffRow, HunkDecision
//
// Integration seam (I2):
//   let mut schemas = api_contract_openapi_components();
//   for (k, v) in run_changes_contract_components() { schemas.insert(k, v); }
// And in document.rs / request_schemas.rs / response_schemas.rs,
// add calls to `run_changes_contract_request_schema` /
// `run_changes_contract_response_schema`.

pub(crate) fn run_changes_contract_components() -> serde_json::Map<String, serde_json::Value> {
    crate::api_run_changes::run_changes_openapi_components()
}

pub(crate) fn run_changes_contract_request_schema(
    method: &str,
    path: &str,
) -> Option<serde_json::Value> {
    crate::api_run_changes::run_changes_openapi_request_schema(method, path)
}

pub(crate) fn run_changes_contract_response_schema(
    method: &str,
    path: &str,
) -> Option<serde_json::Value> {
    crate::api_run_changes::run_changes_openapi_response_schema(method, path)
}
