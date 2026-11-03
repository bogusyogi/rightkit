//! Compiled, disabled reference workflow fixtures.

#[derive(Debug, Clone, Copy)]
pub struct ReferenceWorkflow {
    pub name: &'static str,
    pub yaml: &'static str,
}

pub const WORKFLOWS: &[ReferenceWorkflow] = &[
    ReferenceWorkflow {
        name: "morning-briefing",
        yaml: include_str!("../../fixtures/reference-workflows/01-morning-briefing.yaml"),
    },
    ReferenceWorkflow {
        name: "overnight-pr-triage",
        yaml: include_str!("../../fixtures/reference-workflows/02-overnight-pr-triage.yaml"),
    },
    ReferenceWorkflow {
        name: "document-onboarding",
        yaml: include_str!("../../fixtures/reference-workflows/03-document-onboarding.yaml"),
    },
    ReferenceWorkflow {
        name: "vendor-quiet-hours",
        yaml: include_str!("../../fixtures/reference-workflows/04-vendor-channel-quiet-hours.yaml"),
    },
    ReferenceWorkflow {
        name: "budget-watch",
        yaml: include_str!("../../fixtures/reference-workflows/05-budget-watch.yaml"),
    },
    ReferenceWorkflow {
        name: "seo-audit-content",
        yaml: include_str!("../../fixtures/reference-workflows/seo-audit-content.workflow.yaml"),
    },
    ReferenceWorkflow {
        name: "github-issue-to-pr",
        yaml: include_str!("../../fixtures/reference-workflows/github-issue-to-pr.workflow.yaml"),
    },
];

pub const TRACES: &[&str] = &[
    include_str!(
        "../../fixtures/reference-workflows/expected-traces/morning-briefing.success.json"
    ),
    include_str!(
        "../../fixtures/reference-workflows/expected-traces/overnight-pr-triage.escalated.json"
    ),
    include_str!(
        "../../fixtures/reference-workflows/expected-traces/vendor-quiet-hours.deferred.json"
    ),
];

/// Reference fixtures must remain dogfood-only & draft-safe.
pub fn validate_fixture(workflow: &ReferenceWorkflow) -> Result<(), &'static str> {
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(workflow.yaml).map_err(|_| "reference workflow YAML is invalid")?;
    if !parsed.is_mapping() {
        return Err("reference workflow root must be a mapping");
    }
    let lower = workflow.yaml.to_ascii_lowercase();
    if lower.contains("enabled: true") || lower.contains("enabled: yes") {
        return Err("reference workflow must be disabled");
    }
    for forbidden in [
        "effect.publish",
        "effect.merge",
        "effect.deploy",
        "effect.delete",
    ] {
        if lower.contains(forbidden) {
            return Err("forbidden effect in reference workflow");
        }
    }
    Ok(())
}
