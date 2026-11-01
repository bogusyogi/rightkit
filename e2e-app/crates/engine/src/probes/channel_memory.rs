//! rightkit-channel-memory: parse and validate real channel-memory.v1 documents (the repository's contract fixtures,
//! embedded at build time), cite a proof exactly, and refuse a malformed document at each validation stage.
use rightkit_channel_memory::{parse, Citation, DateWindow, Decimal, MemoryError, ReasonCode, Scope};
use serde_json::{json, Value};

use super::Ctx;

const PROOF_BANK: &str = include_str!("../../../../../contracts/fixtures/channel-memory/valid/proof-bank-rev2.json");
const BAD_DECIMAL: &str =
    include_str!("../../../../../contracts/fixtures/channel-memory/invalid/decimal-value-as-number.json");
const BAD_SEMANTIC: &str =
    include_str!("../../../../../contracts/fixtures/channel-memory/semantic-invalid/claim-value-not-proof-value.json");
const BAD_VERSION: &str = include_str!("../../../../../contracts/fixtures/channel-memory/version-rejected/contract-v2.json");

const TODAY: &str = "2026-10-11";

fn scope(channel: &str) -> Scope {
    Scope { venture: "orthic".into(), channel: channel.into(), account: "main".into() }
}

fn citation(value: &str) -> Result<Citation, String> {
    Ok(Citation {
        proof_id: "proof.receipt-launch-week".into(),
        proof_revision: 2,
        claim_id: "claim.launch-week-revenue".into(),
        value: Decimal::parse(value).ok_or_else(|| format!("decimal {value} did not parse"))?,
        units: "USD".into(),
        subject: "subject.launch-week-revenue".into(),
        period: DateWindow { from: "2026-03-01".into(), to: "2026-03-07".into() },
    })
}

fn refused(error: MemoryError) -> Result<ReasonCode, String> {
    match error {
        MemoryError::Refused { code, .. } => Ok(code),
        other => Err(format!("expected a typed refusal, got {other:?}")),
    }
}

pub fn probe(_ctx: &Ctx) -> Result<Value, String> {
    // A well-formed document parses into the typed model and survives a serde round trip unchanged.
    let memory = parse(PROOF_BANK).map_err(|e| format!("valid proof bank refused: {e}"))?;
    if memory.contract != "channel-memory" || memory.contract_version != "1.0.0" {
        return Err(format!("unexpected contract {} {}", memory.contract, memory.contract_version));
    }
    let text = serde_json::to_string(&memory).map_err(|e| e.to_string())?;
    let again = parse(&text).map_err(|e| format!("round trip refused: {e}"))?;
    if again != memory {
        return Err("the document changed across a serde round trip".into());
    }

    // Scope isolation: the document's own scope passes, another channel is refused before the bank is read.
    memory.ensure_scope(&scope("legion-notes")).map_err(|e| format!("own scope refused: {e}"))?;
    let foreign = memory.ensure_scope(&scope("kids-cartoon")).err().ok_or("a foreign scope was accepted")?;
    if refused(foreign)? != ReasonCode::ForeignScope {
        return Err("a foreign scope was refused with the wrong code".into());
    }

    // Exact citation: the stored value passes (also written without trailing zeros), a different number is refused.
    let proof = memory
        .cite_proof(&scope("legion-notes"), &citation("1240.00")?, TODAY)
        .map_err(|e| format!("exact citation refused: {e}"))?;
    let claim = proof.permitted_claims.first().map(|c| c.text.clone()).ok_or("proof has no permitted claim")?;
    if claim != "Made $1,240 in launch week." {
        return Err(format!("unexpected permitted claim {claim:?}"));
    }
    memory.cite_proof(&scope("legion-notes"), &citation("1240")?, TODAY).map_err(|e| format!("1240 != 1240.00: {e}"))?;
    let wrong = memory.cite_proof(&scope("legion-notes"), &citation("1420.00")?, TODAY).err().ok_or("1420.00 was accepted")?;
    if refused(wrong)? != ReasonCode::NumberMismatch {
        return Err("a wrong number was refused with the wrong code".into());
    }

    // Malformed documents are refused at the stage that owns the defect.
    match parse(BAD_DECIMAL) {
        Err(MemoryError::Schema { errors }) if !errors.is_empty() => {}
        other => return Err(format!("numeric proof value: expected a schema refusal, got {other:?}")),
    }
    match parse(BAD_SEMANTIC) {
        Err(MemoryError::Semantic { .. }) => {}
        other => return Err(format!("claim/proof mismatch: expected a semantic refusal, got {other:?}")),
    }
    match parse(BAD_VERSION) {
        Err(MemoryError::UnsupportedVersion { .. }) => {}
        other => return Err(format!("contract v2: expected a version refusal, got {other:?}")),
    }
    match parse("{ not json") {
        Err(MemoryError::Parse { .. }) => {}
        other => return Err(format!("non-JSON text: expected a parse refusal, got {other:?}")),
    }

    Ok(json!({
        "memoryId": memory.memory_id,
        "revision": memory.revision,
        "proofs": memory.proofs.len(),
        "citedProof": proof.id,
        "refusals": ["foreign_scope", "number_mismatch", "schema", "semantic", "version", "parse"],
    }))
}
