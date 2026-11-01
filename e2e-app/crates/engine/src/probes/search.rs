//! rightkit-search: parse a query with operators, match it with Unicode folding and mid-word fragments, AND across haystacks, and
//! refuse nothing that is plain text.
use rightkit_search::{fold, Query, Syntax, TextMatcher};
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let syntax = Syntax::new(["from", "label"]);
    let m = |q: &str| -> Result<TextMatcher, String> {
        let query = Query::parse(q, &syntax).map_err(|e| format!("parse {q:?}: {e:?}"))?;
        TextMatcher::new(&query).map_err(|e| format!("matcher {q:?}: {e:?}"))
    };
    let cases = [
        ("Café", "CAFÉ", true),
        ("anscr", "Transcript", true),
        ("ÉCOLE", "une école", true),
        ("alpha beta", "alpha only", false),
    ];
    let mut out = vec![];
    for (q, text, want) in cases {
        let got = m(q)?.matches_with(&[text], Vec::new);
        if got != want {
            return Err(format!(
                "{q:?} against {text:?} matched {got}, expected {want}"
            ));
        }
        out.push(json!({ "q": q, "text": text, "match": got }));
    }
    let across = m("alpha beta")?.matches_with(&["alpha here"], || vec!["beta there".to_string()]);
    if !across || fold::fold("ÉCOLE") != "école" {
        return Err(format!(
            "AND across haystacks {across}, fold {}",
            fold::fold("ÉCOLE")
        ));
    }
    Ok(json!({ "cases": out, "and_across": across }))
}
