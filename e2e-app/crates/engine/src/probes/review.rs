//! rightkit-review: an extension rule registered in a rule registry and evaluated through a rule set against real text subjects:
//! a short post is verified, a long one fails with its reason, and the disposition follows the findings.
use rightkit_review::{
    disposition, ArtifactDescription, ArtifactKind, Declarations, Rule, RuleKind, RuleRegistry, RuleSet, Subject, TextBody, Verdict,
};
use serde_json::{json, Value};

fn text(body: &str) -> Subject {
    Subject {
        artifact: ArtifactDescription {
            artifact_id: "post.probe".into(),
            kind: ArtifactKind::Text,
            width: None,
            height: None,
            duration_ms: None,
            frames: None,
            max_flashes_per_second: None,
            text_regions: None,
            determinism: None,
            licence: None,
            text: Some(TextBody { body: body.into(), claims: Vec::new() }),
        },
        declarations: Declarations::default(),
    }
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let mut registry = RuleRegistry::new();
    registry
        .register("rke2e.word_limit", |subject: &Subject, params: &Value| {
            let max = params["max"].as_u64().unwrap_or(0) as usize;
            let words = subject.artifact.text.as_ref().map_or(0, |t| t.body.split_whitespace().count());
            if words <= max { Verdict::Verified } else { Verdict::Failed { reason: format!("{words} words exceeds {max}") } }
        })
        .map_err(|e| format!("register: {e}"))?;
    let set = RuleSet::new("rke2e.text", 1, vec![Rule {
        id: "rke2e.word_limit".into(),
        required: true,
        kind: RuleKind::Extension { kind: "rke2e.word_limit".into(), params: json!({ "max": 5 }) },
    }])
    .map_err(|e| format!("rule set: {e}"))?;
    let short = set.evaluate_with(&text("one two three"), &registry).map_err(|e| format!("evaluate short: {e}"))?;
    let long = set.evaluate_with(&text("one two three four five six seven"), &registry).map_err(|e| format!("evaluate long: {e}"))?;
    let ok = short.iter().all(|f| matches!(f.verdict, Verdict::Verified));
    let failed = long.iter().any(|f| matches!(f.verdict, Verdict::Failed { .. }));
    if !ok || !failed {
        return Err(format!("short verified {ok}, long failed {failed}"));
    }
    Ok(json!({ "short": format!("{:?}", disposition(&short)), "long": format!("{:?}", disposition(&long)) }))
}
