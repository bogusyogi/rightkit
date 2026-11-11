// rightkit-autoedit 0.2.0 consumed by exact registry pin, outside the workspace.
//
// Proves the editing-rules contract a host stores and resolves: a channel rule beats platform,
// content-type and project rules for the same parameter, every loser is reported as overridden by
// the winner, and the resolution is clean.
// Does not prove: cut planning against real transcripts, variants, or renders (need FFmpeg).
use rightkit_autoedit::rules::{
    Condition, Context, Effect, Parameter, Provenance, Rule, RuleMatrix, Scope, Value,
};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn rule(id: &str, scope: Scope, ms: i64) -> Rule {
    Rule {
        id: id.to_owned(),
        scope,
        condition: Condition::Always,
        effect: Effect {
            parameter: Parameter::PadBeforeMs,
            value: Value::Int(ms),
        },
        priority: 0,
        provenance: Provenance {
            set_by: "consumer".into(),
            set_at: "2026-11-01T00:00:00Z".into(),
            note: None,
            source: None,
            edit_id: None,
        },
    }
}

fn main() {
    let matrix = RuleMatrix::from_rules(vec![
        rule("project", Scope::Project("p1".into()), 10),
        rule("platform", Scope::Platform("shorts".into()), 20),
        rule("content", Scope::ContentType("talking_head".into()), 30),
        rule("channel", Scope::Channel("c1".into()), 40),
    ])
    .expect("valid rules");
    let ctx = Context {
        project: "p1".into(),
        channel: Some("c1".into()),
        content_type: Some("talking_head".into()),
        platform: Some("shorts".into()),
    };
    let res = matrix.resolve(&ctx);
    check(res.is_clean(), "the resolution is clean");
    let winner = &res.effective[&Parameter::PadBeforeMs];
    check(
        winner.rule_id == "channel" && winner.value == Value::Int(40),
        "the channel rule wins",
    );
    check(
        res.overridden.len() == 3
            && res
                .overridden
                .iter()
                .all(|o| o.by == vec!["channel".to_owned()]),
        "every other rule is overridden by it",
    );
    println!("rightkit-autoedit consumer: all checks passed");
}
