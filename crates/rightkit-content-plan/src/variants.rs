//! Typed briefs and the platform-native variants derived from a brief, over a host-implemented
//! [`VariantStore`]. This module makes NO network call and publishes nothing.
//!
//! Brief types: seven, each with its own required fields. An explainer needs all five beats (question, model,
//! proof, turn, payoff), each with a scene spec. A product demo needs at least one demo step. Every brief needs a
//! non-empty hook and body. Explainer beats are refused on other types, and demo steps on other types. A refused
//! write stores nothing.
//!
//! Platform rules: this module stores none. The sourced catalog the host supplies
//! ([`VariantStore::catalog`], `rightkit-platform-rules`) is the only rule source: caption length, hashtag
//! count, link placement and duration limits are catalog records, and every variant is bound to the
//! platform's catalog revision id (`rule_revision`). A platform with no catalog record is `rule_missing`.
//!
//! Derivation: `variant.derive` builds one caption per requested platform from the brief at its current revision,
//! under the current catalog revision of that platform. The caption is the hook, a blank line, the body, then the
//! link when the catalog places it in the caption, then the hashtags. Caption length is the number of Unicode
//! scalar values (`chars().count()`) of that final text. A caption over the limit is `caption_too_long`; nothing
//! is truncated. Every platform is checked before any variant is stored, so the first failure stores nothing.
//!
//! Idempotence: a variant id is derived from scope, brief, brief revision, platform, catalog revision, placement,
//! caption and link. The same inputs return the stored variant with `created` false.
//!
//! Scope: every brief and variant row is keyed by venture, channel and account, and every command checks its
//! scope against a stored channel (`foreign_scope` otherwise). Time comes only from `now`.
use rightkit_platform_rules::{
    Catalog, Limit, LinkPlacement, Planned, Status as RuleStatus, Violation,
};
use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::protocol::PLATFORMS;
use rightkit_social_publish::util::{
    bad, in_txn, parse_input, require_channel, to_value, WriteTxn,
};
use serde_json::Value;

use crate::protocol::{
    BriefDetail, BriefVariant, DemoStep, DerivedVariant, ExplainerBeats, ExplainerBeatsInput,
    SceneSpec, Scope, TypedBrief, TypedBriefGetInput, TypedBriefListInput, TypedBriefListOutput,
    TypedBriefPutInput, VariantDeriveInput, VariantDeriveOutput, VariantListInput,
    VariantListOutput, BRIEF_TYPES, EXPLAINER_BEATS,
};
use crate::Fail;

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: &[&str] = &["typed_brief.put", "variant.derive"];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "typed_brief.put",
    "typed_brief.get",
    "typed_brief.list",
    "variant.derive",
    "variant.list",
];

fn invariant(msg: &str) -> Fail {
    ("db_error", format!("store invariant broken: {msg}"))
}

fn conflict(msg: impl Into<String>) -> Fail {
    ("revision_conflict", msg.into())
}

/// What the host app provides: the typed-brief and variant tables and the platform-rule catalog. Every method
/// is one read or one write on the host's own database, on the caller's transaction. The host stores a brief's
/// explainer beats and demo steps as it likes (for example as [`BriefDetail`] JSON).
pub trait VariantStore: WriteTxn {
    /// The sourced platform-rule catalog variants are checked against.
    fn catalog(&self) -> &Catalog;
    /// One typed brief in scope.
    fn load_brief(&self, scope: &Scope, brief_id: &str) -> Result<Option<TypedBrief>, Fail>;
    /// Stores a new brief (revision 1; `created_at` and `updated_at` are the brief's).
    fn insert_brief(&self, brief: &TypedBrief) -> Result<(), Fail>;
    /// Overwrites type, hook, body, detail, revision and `updated_at` of the brief where the stored revision
    /// equals `expected_revision`. Returns whether a row changed.
    fn update_brief(&self, brief: &TypedBrief, expected_revision: i64) -> Result<bool, Fail>;
    /// Every brief in scope, ordered by brief id.
    fn list_briefs(&self, scope: &Scope) -> Result<Vec<TypedBrief>, Fail>;
    /// One variant in scope.
    fn load_variant(&self, scope: &Scope, variant_id: &str) -> Result<Option<BriefVariant>, Fail>;
    /// Stores one variant.
    fn insert_variant(&self, variant: &BriefVariant) -> Result<(), Fail>;
    /// The variants of one brief in scope, ordered by platform then variant id.
    fn list_variants(&self, scope: &Scope, brief_id: &str) -> Result<Vec<BriefVariant>, Fail>;
}

fn slug(value: &str, name: &str) -> Result<(), Fail> {
    let ok = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "{name} must be a lower-case slug of 1 to 64 characters from a-z, 0-9, _ and -, got {value}"
        )))
    }
}

/// The trimmed text, refused when blank.
fn checked_text(value: &str, name: &str) -> Result<String, Fail> {
    let text = value.trim();
    if text.is_empty() {
        Err(bad(format!("{name} must not be empty")))
    } else {
        Ok(text.to_string())
    }
}

fn one_of(value: &str, allowed: &[&str], name: &str) -> Result<(), Fail> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(bad(format!(
            "{name} must be one of {}, got {value}",
            allowed.join(", ")
        )))
    }
}

/// A platform is one of the account vocabulary, `PLATFORMS`. Rules are for account platforms only.
fn platform_ok(platform: &str) -> Result<(), Fail> {
    if PLATFORMS.contains(&platform) {
        Ok(())
    } else {
        Err(bad(format!(
            "platform must be one of {}, got {platform}",
            PLATFORMS.join(", ")
        )))
    }
}

fn at_least(value: i64, min: i64, name: &str) -> Result<(), Fail> {
    if value < min {
        Err(bad(format!("{name} must be at least {min}, got {value}")))
    } else {
        Ok(())
    }
}

/// Trimmed text, or None when absent or blank.
fn trimmed_opt(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The clock for this command: the required `now` input, as stored text. The system clock is never read.
fn stamp_of(now: Option<&str>) -> Result<String, Fail> {
    let raw = now.ok_or_else(|| bad("now is required: a UTC instant YYYY-MM-DDTHH:MM:SSZ"))?;
    Ok(time::format_instant(time::parse_instant(raw, "now")?))
}

/// The https check for a link. Refuses whitespace, a non-https scheme, a private or loopback host, and any
/// user, port or credential part (`links::public_https`), so the host is the only host the URL names.
fn https_url(raw: &str, name: &str) -> Result<String, Fail> {
    let url = raw.trim();
    rightkit_social_analytics::links::public_https(url, name)?;
    Ok(url.to_string())
}

/// A hashtag as stored: one word with no spaces, with exactly one leading `#`.
fn hashtag(raw: &str) -> Result<String, Fail> {
    let name = raw.trim().trim_start_matches('#');
    if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad(format!(
            "a hashtag must be one word without spaces, got {raw}"
        )));
    }
    Ok(format!("#{name}"))
}

/// The deterministic variant id for one set of inputs: `var_` plus 32 hex digits (SHA-256, first 128 bits).
pub fn variant_id(parts: &[&str]) -> String {
    let mut bytes = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            bytes.push(0x1f);
        }
        bytes.extend_from_slice(part.as_bytes());
    }
    format!("var_{}", &rightkit_fs::digest::sha256_bytes(&bytes)[..32])
}

fn brief_not_found(brief_id: &str) -> Fail {
    (
        "not_found",
        format!("no typed brief {brief_id} for this scope"),
    )
}

/// One scene of an explainer beat, checked and trimmed.
fn checked_scene(scene: Option<SceneSpec>, beat: &str) -> Result<SceneSpec, Fail> {
    let scene = scene.ok_or_else(|| {
        bad(format!(
            "explainer beat {beat} is missing: each beat needs a scene spec"
        ))
    })?;
    Ok(SceneSpec {
        teaches: checked_text(&scene.teaches, &format!("explainer beat {beat} teaches"))?,
        frame: checked_text(&scene.frame, &format!("explainer beat {beat} frame"))?,
        motion: checked_text(&scene.motion, &format!("explainer beat {beat} motion"))?,
        words: checked_text(&scene.words, &format!("explainer beat {beat} words"))?,
        sound: checked_text(&scene.sound, &format!("explainer beat {beat} sound"))?,
    })
}

/// The five beats in story order. The first missing beat is named.
fn checked_beats(beats: ExplainerBeatsInput) -> Result<ExplainerBeats, Fail> {
    Ok(ExplainerBeats {
        question: checked_scene(beats.question, "question")?,
        model: checked_scene(beats.model, "model")?,
        proof: checked_scene(beats.proof, "proof")?,
        turn: checked_scene(beats.turn, "turn")?,
        payoff: checked_scene(beats.payoff, "payoff")?,
    })
}

/// The type-specific detail of a brief. Explainer beats are required on explainers and refused elsewhere;
/// demo steps are required on product demos and refused elsewhere.
fn checked_detail(
    brief_type: &str,
    explainer: Option<ExplainerBeatsInput>,
    demo_steps: Vec<DemoStep>,
) -> Result<BriefDetail, Fail> {
    let explainer = match (brief_type, explainer) {
        ("explainer", Some(beats)) => Some(checked_beats(beats)?),
        ("explainer", None) => {
            return Err(bad(format!(
                "an explainer needs its five beats ({}), each with a scene spec",
                EXPLAINER_BEATS.join(", ")
            )))
        }
        (_, Some(_)) => {
            return Err(bad(format!(
                "explainer beats belong only to explainer briefs, not {brief_type}"
            )))
        }
        (_, None) => None,
    };
    let demo_steps = if brief_type == "product_demo" {
        if demo_steps.is_empty() {
            return Err(bad("a product_demo needs at least one demo step"));
        }
        let mut steps = Vec::new();
        for (i, step) in demo_steps.iter().enumerate() {
            steps.push(DemoStep {
                say: checked_text(&step.say, &format!("demo step {} say", i + 1))?,
            });
        }
        steps
    } else {
        if !demo_steps.is_empty() {
            return Err(bad(format!(
                "demo steps belong only to product_demo briefs, not {brief_type}"
            )));
        }
        Vec::new()
    };
    Ok(BriefDetail {
        explainer,
        demo_steps,
    })
}

struct Placed {
    in_caption: Option<String>,
    first_comment: Option<String>,
    bio_link: Option<String>,
}

/// The UTC day (`YYYY-MM-DD`) of `now`, the date every catalog lookup is made on.
fn today_of(now: Option<&str>) -> Result<String, Fail> {
    Ok(time::format_instant(time::now_secs(now)?)[..10].to_owned())
}

/// The refusal of a catalog violation that is not a limit breach.
fn violation_fail(platform: &str, v: &Violation) -> Fail {
    let code = match v {
        Violation::Blocked {
            status: RuleStatus::Stale,
            ..
        } => "rule_stale",
        Violation::Blocked { .. } => "rule_unsourced",
        _ => "rule_violation",
    };
    (code, format!("{platform}: {v}"))
}

/// The placement of the link for a platform, from the catalog's `link.placement` record: `caption` when the
/// catalog allows it, else `first_comment`, else `bio`, else `none`. A platform with no placement record keeps
/// the default, `caption`.
fn link_placement(catalog: &Catalog, platform: &str, today: &str) -> Result<String, Fail> {
    match catalog.link_placements_at(platform, "all", today) {
        Ok(allowed) => {
            let pick = [
                LinkPlacement::Caption,
                LinkPlacement::FirstComment,
                LinkPlacement::Bio,
            ]
            .into_iter()
            .find(|p| allowed.contains(p))
            .unwrap_or(LinkPlacement::NoLink);
            Ok(pick.as_str().to_owned())
        }
        Err(Violation::NoRule { .. }) => Ok(LinkPlacement::Caption.as_str().to_owned()),
        Err(v) => Err(violation_fail(platform, &v)),
    }
}

/// Places the link under the catalog placement. A platform that places no link refuses a given link: it is never
/// dropped silently.
fn place_link(platform: &str, placement: &str, link: Option<&str>) -> Result<Placed, Fail> {
    let Some(link) = link else {
        return Ok(Placed::default());
    };
    let mut placed = Placed::default();
    let slot = match placement {
        "caption" => &mut placed.in_caption,
        "first_comment" => &mut placed.first_comment,
        "bio" => &mut placed.bio_link,
        _ => {
            return Err((
                "link_not_placeable",
                format!("{platform} has link placement {placement}; derive it without a link"),
            ))
        }
    };
    *slot = Some(link.to_string());
    Ok(placed)
}

/// Hook, blank line, body; then the link when it sits in the caption; then the hashtags.
fn build_caption(brief: &TypedBrief, in_caption: Option<&str>, hashtags: &[String]) -> String {
    let mut caption = format!("{}\n\n{}", brief.hook, brief.body);
    if let Some(link) = in_caption {
        caption.push_str("\n\n");
        caption.push_str(link);
    }
    if !hashtags.is_empty() {
        caption.push_str("\n\n");
        caption.push_str(&hashtags.join(" "));
    }
    caption
}

/// The age check of `variant.derive`: a platform whose oldest current catalog record was fetched more than
/// `max_days` days before the clock is refused with `rule_stale`, and nothing is stored. A null limit checks
/// nothing.
fn check_rule_age(
    catalog: &Catalog,
    platform: &str,
    max_days: Option<i64>,
    now: Option<&str>,
) -> Result<(), Fail> {
    let Some(days) = max_days else {
        return Ok(());
    };
    at_least(days, 0, "max_rule_age_days")?;
    let Some(oldest) = catalog
        .rules()
        .iter()
        .filter(|r| r.platform == platform)
        .filter(|r| {
            catalog
                .get(&r.platform, &r.account_type, &r.capability, &r.format)
                .is_some_and(|latest| latest.rule_revision == r.rule_revision)
        })
        .map(|r| r.fetched())
        .min()
    else {
        return Ok(());
    };
    let fetched = time::parse_instant(&format!("{oldest}T00:00:00Z"), "fetched_date")?;
    let now_secs = time::now_secs(now)?;
    if now_secs - fetched > days * 86_400 {
        let age_days = (now_secs - fetched).div_euclid(86_400);
        return Err((
            "rule_stale",
            format!(
                "catalog rules for {platform} were fetched {oldest} (about {age_days} days before now); the limit is {days} days; refresh rightkit-platform-rules"
            ),
        ));
    }
    Ok(())
}

/// Checks one variant against the sourced catalog rules of its platform, bound to the platform's current rule
/// revision: the caption length (`caption.length`, else `text.length`, in characters), the hashtag count
/// (`caption.hashtags`) and, when a positive duration is given, `video.duration` (reel, else video) in the
/// rule's own unit. A rule that is unsourced or past its sunset refuses.
fn check_catalog(
    catalog: &Catalog,
    platform: &str,
    caption: &str,
    hashtags: usize,
    duration: Option<i64>,
    rule_revision: &str,
    now: Option<&str>,
) -> Result<(), Fail> {
    let today = today_of(now)?;
    let mut checks: Vec<(&'static str, Planned)> = Vec::new();
    let length = caption.chars().count() as f64;
    for capability in ["caption.length", "text.length"] {
        if let Some(rule) = catalog.get(platform, "all", capability, "text") {
            if rule.unit.as_deref() == Some("characters") {
                checks.push((
                    "caption_too_long",
                    Planned::number("all", capability, "text", length).in_unit("characters"),
                ));
            }
            break;
        }
    }
    match catalog.hashtag_max_at(platform, "all", &today) {
        Ok(max) if hashtags > max as usize => {
            return Err((
                "hashtags_too_many",
                format!("{platform}: {hashtags} is above the maximum {max} hashtags"),
            ))
        }
        Ok(_) | Err(Violation::NoRule { .. }) => {}
        Err(v) => return Err(violation_fail(platform, &v)),
    }
    if let Some(seconds) = duration.filter(|s| *s > 0) {
        for (account, format) in [("all", "reel"), ("default", "video")] {
            let Some(rule) = catalog.get(platform, account, "video.duration", format) else {
                continue;
            };
            let planned = match rule.unit.as_deref() {
                Some("seconds") => seconds as f64,
                Some("minutes") => seconds as f64 / 60.0,
                _ => break,
            };
            if !matches!(rule.limit(), Limit::Unstated) {
                let unit = rule.unit.clone().unwrap_or_default();
                checks.push((
                    "duration_too_long",
                    Planned::number(&rule.account_type, "video.duration", &rule.format, planned)
                        .in_unit(&unit),
                ));
            }
            break;
        }
    }
    for (code, planned) in checks {
        catalog
            .check_id_at(rule_revision, std::slice::from_ref(&planned), &today)
            .map_err(|v| {
                let code = match &v {
                    Violation::AboveMax { .. } => code,
                    Violation::BelowMin { .. } => "duration_too_short",
                    Violation::Blocked {
                        status: RuleStatus::Stale,
                        ..
                    } => "rule_stale",
                    Violation::Blocked { .. } => "rule_unsourced",
                    _ => "rule_violation",
                };
                (code, format!("{platform}: {v}"))
            })?;
    }
    Ok(())
}

/// Route one method of this module. Unknown methods are `unknown_method`.
pub fn dispatch(store: &dyn VariantStore, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "typed_brief.put" => brief_put(store, input),
        "typed_brief.get" => brief_get(store, input),
        "typed_brief.list" => brief_list(store, input),
        "variant.derive" => variant_derive(store, input),
        "variant.list" => variant_list(store, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// `typed_brief.put`: create or edit one brief. Checks run before any write, so a refused brief stores nothing.
fn brief_put(store: &dyn VariantStore, input: Value) -> Result<Value, Fail> {
    let req: TypedBriefPutInput = parse_input(input, "typed_brief.put")?;
    slug(&req.brief_id, "brief_id")?;
    one_of(&req.brief_type, &BRIEF_TYPES, "brief_type")?;
    let hook = checked_text(&req.hook, "hook")?;
    let body = checked_text(&req.body, "body")?;
    let detail = checked_detail(&req.brief_type, req.explainer, req.demo_steps)?;
    let stamp = stamp_of(req.now.as_deref())?;

    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let brief = |revision: i64, created_at: &str| TypedBrief {
            scope: req.scope.clone(),
            brief_id: req.brief_id.clone(),
            brief_type: req.brief_type.clone(),
            hook: hook.clone(),
            body: body.clone(),
            explainer: detail.explainer.clone(),
            demo_steps: detail.demo_steps.clone(),
            revision,
            created_at: created_at.to_string(),
            updated_at: stamp.clone(),
        };
        match store.load_brief(&req.scope, &req.brief_id)? {
            None => {
                match req.expected_revision {
                    None | Some(0) => {}
                    Some(n) => {
                        return Err(conflict(format!(
                    "typed brief {} does not exist (revision 0); expected_revision {n} is stale",
                    req.brief_id
                )))
                    }
                }
                store.insert_brief(&brief(1, &stamp))?;
            }
            Some(cur) => {
                let expected = req.expected_revision.ok_or_else(|| {
                    (
                        "revision_required",
                        format!(
                            "typed brief {} exists: expected_revision is required",
                            req.brief_id
                        ),
                    )
                })?;
                if cur.revision != expected {
                    return Err(conflict(format!(
                        "typed brief {} is at revision {}; expected_revision {expected} is stale",
                        req.brief_id, cur.revision
                    )));
                }
                if !store.update_brief(&brief(cur.revision + 1, &cur.created_at), expected)? {
                    return Err(conflict(format!(
                        "typed brief {} changed while it was written",
                        req.brief_id
                    )));
                }
            }
        }
        store
            .load_brief(&req.scope, &req.brief_id)?
            .ok_or_else(|| invariant("typed brief vanished after write"))
    })?;
    to_value(&saved)
}

/// `typed_brief.get`: one brief in scope.
fn brief_get(store: &dyn VariantStore, input: Value) -> Result<Value, Fail> {
    let req: TypedBriefGetInput = parse_input(input, "typed_brief.get")?;
    slug(&req.brief_id, "brief_id")?;
    require_channel(store, &req.scope)?;
    let brief = store
        .load_brief(&req.scope, &req.brief_id)?
        .ok_or_else(|| brief_not_found(&req.brief_id))?;
    to_value(&brief)
}

/// `typed_brief.list`: every brief in one scope, ordered by brief id.
fn brief_list(store: &dyn VariantStore, input: Value) -> Result<Value, Fail> {
    let req: TypedBriefListInput = parse_input(input, "typed_brief.list")?;
    require_channel(store, &req.scope)?;
    let briefs = store.list_briefs(&req.scope)?;
    to_value(&TypedBriefListOutput {
        scope: req.scope,
        briefs,
    })
}

/// `variant.derive`: one variant per platform from the brief at its current revision. Every platform is
/// checked before any variant is stored.
fn variant_derive(store: &dyn VariantStore, input: Value) -> Result<Value, Fail> {
    let req: VariantDeriveInput = parse_input(input, "variant.derive")?;
    slug(&req.brief_id, "brief_id")?;
    if req.platforms.is_empty() {
        return Err(bad("platforms must name at least one platform"));
    }
    for (i, platform) in req.platforms.iter().enumerate() {
        platform_ok(platform)?;
        if req.platforms[..i].contains(platform) {
            return Err(bad(format!("platform {platform} is named twice")));
        }
    }
    let mut hashtags: Vec<String> = Vec::new();
    for raw in &req.hashtags {
        let tag = hashtag(raw)?;
        if hashtags.contains(&tag) {
            return Err(bad(format!("hashtag {tag} is named twice")));
        }
        hashtags.push(tag);
    }
    let link = trimmed_opt(req.link.as_deref())
        .map(|url| https_url(&url, "link"))
        .transpose()?;
    if let Some(seconds) = req.duration_seconds {
        at_least(seconds, 0, "duration_seconds")?;
    }
    let stamp = stamp_of(req.now.as_deref())?;
    let catalog = store.catalog();

    let variants = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let brief = store
            .load_brief(&req.scope, &req.brief_id)?
            .ok_or_else(|| brief_not_found(&req.brief_id))?;

        let mut candidates: Vec<BriefVariant> = Vec::new();
        for platform in &req.platforms {
            if !catalog.rules().iter().any(|r| &r.platform == platform) {
                return Err((
                    "rule_missing",
                    format!(
                        "no platform rules for {platform} in the rightkit-platform-rules catalog"
                    ),
                ));
            }
            check_rule_age(catalog, platform, req.max_rule_age_days, req.now.as_deref())?;
            let today = today_of(req.now.as_deref())?;
            let rule_revision = catalog.revision_id(platform);
            let placement = link_placement(catalog, platform, &today)?;
            let placed = place_link(platform, &placement, link.as_deref())?;
            let caption = build_caption(&brief, placed.in_caption.as_deref(), &hashtags);
            check_catalog(
                catalog,
                platform,
                &caption,
                hashtags.len(),
                req.duration_seconds,
                &rule_revision,
                req.now.as_deref(),
            )?;
            let revision = brief.revision.to_string();
            let id = variant_id(&[
                req.scope.venture.as_str(),
                req.scope.channel.as_str(),
                req.scope.account.as_str(),
                req.brief_id.as_str(),
                revision.as_str(),
                platform.as_str(),
                rule_revision.as_str(),
                placement.as_str(),
                caption.as_str(),
                link.as_deref().unwrap_or(""),
            ]);
            candidates.push(BriefVariant {
                scope: req.scope.clone(),
                variant_id: id,
                brief_id: req.brief_id.clone(),
                brief_revision: brief.revision,
                platform: platform.clone(),
                rule_revision,
                link_placement: placement,
                caption,
                link: link.clone(),
                first_comment: placed.first_comment,
                bio_link: placed.bio_link,
                created_at: stamp.clone(),
            });
        }

        let mut variants = Vec::new();
        for candidate in candidates {
            if let Some(stored) = store.load_variant(&candidate.scope, &candidate.variant_id)? {
                variants.push(DerivedVariant {
                    variant: stored,
                    created: false,
                });
                continue;
            }
            store.insert_variant(&candidate)?;
            let saved = store
                .load_variant(&candidate.scope, &candidate.variant_id)?
                .ok_or_else(|| invariant("variant vanished after insert"))?;
            variants.push(DerivedVariant {
                variant: saved,
                created: true,
            });
        }
        Ok(variants)
    })?;
    to_value(&VariantDeriveOutput {
        scope: req.scope,
        brief_id: req.brief_id,
        variants,
    })
}

/// `variant.list`: the variants of one brief in one scope, ordered by platform then variant id.
fn variant_list(store: &dyn VariantStore, input: Value) -> Result<Value, Fail> {
    let req: VariantListInput = parse_input(input, "variant.list")?;
    slug(&req.brief_id, "brief_id")?;
    require_channel(store, &req.scope)?;
    let variants = store.list_variants(&req.scope, &req.brief_id)?;
    to_value(&VariantListOutput {
        scope: req.scope,
        brief_id: req.brief_id,
        variants,
    })
}
