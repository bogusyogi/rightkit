use rightkit_evidence_contracts::{canonical_json_bytes, ReportV1, ReportV2};

use crate::{
    error::{validate, ReportError},
    ordered_findings,
};

pub fn render(report: &ReportV1) -> Result<String, ReportError> {
    validate(report)?;
    let mut value = serde_json::to_value(report)?;
    if let Some(findings) = value
        .get_mut("findings")
        .and_then(serde_json::Value::as_array_mut)
    {
        let ordered = ordered_findings(report)
            .into_iter()
            .map(|finding| serde_json::to_value(finding).map_err(ReportError::from))
            .collect::<Result<Vec<_>, _>>()?;
        *findings = ordered;
    }
    let bytes = canonical_json_bytes(&value)
        .map_err(|error| ReportError::Serialization(error.to_string()))?;
    String::from_utf8(bytes).map_err(|_| ReportError::InvalidUtf8)
}

pub fn to_json(report: &ReportV1) -> Result<String, ReportError> {
    render(report)
}

/// Renders a v2 report at the requested schema: `2` natively, `1` as the
/// compatible projection (`--report-schema 1`). Findings are ordered like
/// [`render`].
pub fn render_schema(report: &ReportV2, schema: u32) -> Result<String, ReportError> {
    let mut ordered = report.clone();
    ordered.findings.sort_by(|left, right| {
        let key = |finding: &rightkit_evidence_contracts::FindingV2| {
            let core = finding.v1_core();
            (
                core.id.as_str().to_owned(),
                crate::first_location(&core),
                core.message,
            )
        };
        key(left).cmp(&key(right))
    });
    let value = ordered
        .to_value_for_schema(schema)
        .map_err(|error| ReportError::Contract(error.to_string()))?;
    let bytes = canonical_json_bytes(&value)
        .map_err(|error| ReportError::Serialization(error.to_string()))?;
    String::from_utf8(bytes).map_err(|_| ReportError::InvalidUtf8)
}
