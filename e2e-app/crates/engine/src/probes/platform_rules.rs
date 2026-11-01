//! rightkit-platform-rules: the bundled catalog loads, a known rule (Threads text, 500 characters) is found with its revision, a
//! planned post within the limit passes and one over it is refused.
use rightkit_platform_rules::{Catalog, Planned};
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let catalog = Catalog::bundled();
    if catalog.rules().is_empty() {
        return Err("the bundled catalog has no rules".into());
    }
    let rule = catalog
        .get("threads", "all", "text.length", "text")
        .ok_or("no threads text.length rule in the bundled catalog")?;
    let revision = catalog.revision("threads");
    let ok = catalog.check(
        &revision,
        &[Planned::number("all", "text.length", "text", 480.0)],
    );
    let over = catalog.check(
        &revision,
        &[Planned::number("all", "text.length", "text", 520.0)],
    );
    if ok.is_err() || over.is_ok() {
        return Err(format!(
            "480 chars passed: {}, 520 chars refused: {}",
            ok.is_ok(),
            over.is_err()
        ));
    }
    let round = Catalog::from_json(&catalog.to_json()).map_err(|e| format!("round trip: {e:?}"))?;
    Ok(
        json!({ "rules": catalog.rules().len(), "threads_revision": catalog.revision_id("threads"), "limit": format!("{:?}", rule.limit()), "round_trip_rules": round.rules().len() }),
    )
}
