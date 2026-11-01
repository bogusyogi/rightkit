//! rightkit-contracts: validate a real content brief through every stage (name, version negotiation, JSON Schema,
//! semantic rules, typed view) using the repository's contract fixtures embedded at build time, and refuse a malformed
//! document at each stage. Every one of the seven embedded schemas must compile and agree with its own `$id`.
use rightkit_contracts::types::content_brief::ReviewMode;
use rightkit_contracts::types::ContentBrief;
use rightkit_contracts::{
    check_typed, eyes_gate, negotiate, parse, validate_str, Contract, ContractError, EyesGate, EYES_APPROVAL_REQUIRED,
};
use serde_json::{json, Value};

use super::Ctx;

const VALID: &str = include_str!("../../../../../contracts/fixtures/content-brief/valid/product-demo.json");
const NO_SCENARIO: &str =
    include_str!("../../../../../contracts/fixtures/content-brief/invalid/demo-without-scenario.json");
const DUPLICATE_STEP: &str =
    include_str!("../../../../../contracts/fixtures/content-brief/semantic-invalid/duplicate-step-id.json");
const MAJOR_2: &str = include_str!("../../../../../contracts/fixtures/content-brief/version-rejected/contract-v2.json");
const MINOR_AHEAD: &str =
    include_str!("../../../../../contracts/fixtures/content-brief/version-rejected/contract-minor-ahead.json");

pub fn probe(_ctx: &Ctx) -> Result<Value, String> {
    // All seven contracts: name round trip, an embedded schema that is JSON and names its own id.
    for contract in Contract::ALL {
        if Contract::from_name(contract.name()) != Some(contract) {
            return Err(format!("{} does not round trip through its name", contract.name()));
        }
        let schema: Value = serde_json::from_str(contract.schema_source())
            .map_err(|e| format!("{} embedded schema is not JSON: {e}", contract.name()))?;
        if schema.get("$id").and_then(Value::as_str) != Some(contract.schema_id()) {
            return Err(format!("{} schema $id disagrees with schema_id()", contract.name()));
        }
    }

    // Valid brief: schema + semantics + typed view.
    let document = validate_str(Contract::ContentBrief, VALID).map_err(|e| format!("valid brief refused: {e}"))?;
    check_typed(Contract::ContentBrief, VALID).map_err(|e| format!("typed view refused: {e}"))?;
    let brief: ContentBrief = parse(Contract::ContentBrief, VALID).map_err(|e| format!("parse refused: {e}"))?;
    if brief.brief_id != "brief.demo-export-2026-10" || brief.revision != 2 || brief.supersedes != Some(1) {
        return Err(format!("unexpected brief identity {} r{} supersedes {:?}", brief.brief_id, brief.revision, brief.supersedes));
    }
    if document.get("scenario").and_then(|s| s.get("steps")).and_then(Value::as_array).map(Vec::len) != Some(2) {
        return Err("the valid brief lost its two scenario steps".into());
    }
    // The same document under another contract's name is refused before any schema work.
    match validate_str(Contract::DemoPlan, VALID) {
        Err(ContractError::WrongContract { .. }) => {}
        other => return Err(format!("wrong contract name: got {other:?}")),
    }

    // Each stage refuses its own defect.
    match validate_str(Contract::ContentBrief, NO_SCENARIO) {
        Err(ContractError::Schema { errors, .. }) if !errors.is_empty() => {}
        other => return Err(format!("a demo without a scenario: expected a schema refusal, got {other:?}")),
    }
    match validate_str(Contract::ContentBrief, DUPLICATE_STEP) {
        Err(ContractError::Semantic { .. }) => {}
        other => return Err(format!("duplicate step id: expected a semantic refusal, got {other:?}")),
    }
    match validate_str(Contract::ContentBrief, MAJOR_2) {
        Err(ContractError::IncompatibleMajor { .. }) => {}
        other => return Err(format!("major 2: expected IncompatibleMajor, got {other:?}")),
    }
    match validate_str(Contract::ContentBrief, MINOR_AHEAD) {
        Err(ContractError::NewerMinor { .. }) => {}
        other => return Err(format!("minor 1.3: expected NewerMinor, got {other:?}")),
    }
    match validate_str(Contract::ContentBrief, "{ nope") {
        Err(ContractError::Parse { .. }) => {}
        other => return Err(format!("non-JSON: expected a parse refusal, got {other:?}")),
    }
    // Version negotiation on its own: patch never gates, a newer minor does.
    let supported = Contract::ContentBrief.supported_version();
    negotiate(Contract::ContentBrief, &format!("{}.{}.99", supported.major, supported.minor))
        .map_err(|e| format!("a newer patch was refused: {e}"))?;
    if negotiate(Contract::ContentBrief, &format!("{}.{}.0", supported.major, supported.minor + 1)).is_ok() {
        return Err("a newer minor was accepted".into());
    }

    // The eyes gate fails closed: a manual brief without a person's approval is refused.
    if eyes_gate(ReviewMode::Manual, false, true) != Err(EYES_APPROVAL_REQUIRED) {
        return Err("manual review delivered without a person".into());
    }
    if eyes_gate(ReviewMode::Manual, true, false) != Ok(EyesGate::Approved) {
        return Err("manual review with approval was refused".into());
    }

    Ok(json!({
        "contracts": Contract::ALL.len(),
        "brief": brief.brief_id,
        "contentBriefVersion": supported.to_string(),
        "refusals": ["wrong_contract", "schema", "semantic", "incompatible_major", "newer_minor", "parse", "eyes_gate"],
    }))
}
