// rightkit-platform-rules consumed by the exact registry pin in Cargo.toml, outside the workspace.
//
// Proves the catalog a content planner binds work to, with a fixed check date and no clock:
//   * the bundled catalog loads, keeps every revision, and answers get() with the account-type
//     fallback to "all" and the format fallback to "any" (a Threads text limit, the two X video
//     limits that must not fall through to each other, an absent rule);
//   * check_at: a value at the limit passes, above the maximum, below the minimum, a unit
//     mismatch, a wrong value type, an absent rule, an unsourced rule, a stale revision binding,
//     an enum rule and a boolean rule are each refused with the matching Violation;
//   * the persisted revision id (platform:hash): stable, 32 lowercase hex digits, scoped to its
//     platform, round-trips through RuleRevision::parse / FromStr / Display, a malformed id is
//     BadRevisionId, is_current_id turns false once any rule of the platform changes;
//   * merge and merge_json: a change is a new revision (previous + 1, supersedes naming it), the
//     original catalog is unchanged, history is kept, an edit / a skipped revision / a renamed rule
//     is refused, a source-only change keeps the hash, a new key starts at revision 1;
//   * promotion rules enforced on load (grade, source URL, stated value, conflict note, no value on
//     an unsourced record), shape errors (bad JSON, schema, dates, min above max, empty fields,
//     duplicate revision, broken supersedes chain, unknown link placement);
//   * a sunset date makes a sourced rule stale from the next day (status_at, blocks_use_at,
//     check_at), exclusive bounds, Rule::limit decoding for every limit kind, typed accessors
//     hashtag_max_at and link_placements_at, LinkPlacement names, upload routes (get vs get_in,
//     Planned::on_route, Rule::with_route), to_json / from_json equality, Display of every error.
// Public API only.
// Does not prove: the clock-dated variants check, check_id, hashtag_max and link_placements (they
// read today's UTC date from the system clock, so their result changes with the day), or the
// accuracy of any platform's real-world limit (the data is sourced from research, not tested here).
// State: in-memory strings only. No network, no files, no process spawn, no clock, no user data.
use rightkit_platform_rules::{
    Catalog, CatalogError, Limit, LimitKind, LinkPlacement, Planned, PlannedValue, Rule,
    RuleRevision, SourceGrade, Status, Violation, Window, LINK_PLACEMENTS,
};

const TODAY: &str = "2026-10-10";

fn sourced(id: &str, platform: &str, capability: &str, kind: &str, extra: &str) -> String {
    format!(
        r#"{{"ruleId":"{id}","platform":"{platform}","accountType":"all","capability":"{capability}","format":"text","limitKind":"{kind}",{extra},"sourceUrl":"https://example.test/doc","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-08","ruleRevision":1,"status":"sourced"}}"#
    )
}

fn doc(rules: &[String]) -> String {
    format!(r#"{{"schema":1,"rules":[{}]}}"#, rules.join(","))
}

fn is_error<E: std::error::Error>(_: &E) {}

fn bundled_catalog() {
    let catalog = Catalog::bundled();
    assert!(
        catalog.rules().len() >= 18,
        "the bundled catalog is populated"
    );
    let rule = catalog
        .get("threads", "all", "text.length", "text")
        .expect("threads text length");
    assert_eq!(rule.limit(), Limit::Max(500.0));
    assert_eq!(rule.revision(), 1);
    assert_eq!(rule.fetched(), "2026-10-08");
    assert_eq!(rule.platform, "threads");
    assert_eq!(rule.rule_id, "threads.text.length");
    assert_eq!(rule.unit.as_deref(), Some("characters"));
    assert_eq!(rule.source_grade, SourceGrade::OfficialFetched);
    assert_eq!(rule.status, Status::Sourced);
    assert_eq!(rule.supersedes, None);
    assert!(rule.source_url.is_some(), "a sourced rule names its source");
    assert_eq!(rule.route(), None);

    // threads.publish.window is account "all", format "any": a member account on a text post
    // falls back to both.
    let window = catalog
        .get("threads", "member", "publish.window", "text")
        .expect("fallback to all accounts and any format");
    assert_eq!(window.limit(), Limit::Max(250.0));
    assert_eq!(
        window.window,
        Some(Window {
            length: 24,
            unit: "hours".to_owned()
        })
    );

    // X video limits differ by account type and must not fall through to each other.
    let default = catalog
        .get("x", "default", "video.duration", "video")
        .expect("x default video");
    let premium = catalog
        .get("x", "premium_or_verified", "video.duration", "video")
        .expect("x premium video");
    assert_eq!(default.limit(), Limit::Max(20.0));
    assert_eq!(premium.limit(), Limit::Max(125.0));
    assert!(catalog
        .get("x", "other", "video.duration", "video")
        .is_none());
    assert!(catalog.get("threads", "all", "no.such", "text").is_none());
    assert!(catalog
        .get("no-such-platform", "all", "text.length", "text")
        .is_none());

    // The Instagram caption length keeps revision 1 (unsourced) as history and answers with
    // revision 2 (sourced).
    let caption = catalog
        .get("instagram", "all", "caption.length", "text")
        .expect("instagram caption length");
    assert_eq!(caption.revision(), 2);
    assert_eq!(caption.supersedes, Some(1));
    assert_eq!(caption.limit(), Limit::Max(2200.0));
    let history: Vec<u32> = catalog
        .rules()
        .iter()
        .filter(|r| r.rule_id == "instagram.caption.length")
        .map(Rule::revision)
        .collect();
    assert_eq!(history, vec![1, 2], "every revision is kept, in order");
}

fn checks_against_the_bundled_catalog() {
    let catalog = Catalog::bundled();
    let revision = catalog.revision("threads");

    let at_limit = Planned::number("all", "text.length", "text", 500.0).in_unit("characters");
    assert_eq!(catalog.check_at(&revision, &[at_limit], TODAY), Ok(()));
    let readme_example = Planned::number("all", "text.length", "text", 480.0).in_unit("characters");
    assert_eq!(
        catalog.check_at(&revision, &[readme_example], TODAY),
        Ok(())
    );

    let over = Planned::number("all", "text.length", "text", 501.0);
    match catalog.check_at(&revision, &[over], TODAY) {
        Err(Violation::AboveMax {
            rule_id,
            planned,
            max,
        }) => {
            assert_eq!(rule_id, "threads.text.length");
            assert_eq!((planned, max), (501.0, 500.0));
        }
        other => panic!("501 characters must be above the maximum: {other:?}"),
    }
    let low = Planned::number("all", "carousel.item_count", "carousel", 1.0);
    assert!(matches!(
        catalog.check_at(&revision, &[low], TODAY),
        Err(Violation::BelowMin { min, .. }) if min == 2.0
    ));
    for ok in [2.0, 20.0] {
        let item = Planned::number("all", "carousel.item_count", "carousel", ok);
        assert_eq!(catalog.check_at(&revision, &[item], TODAY), Ok(()));
    }
    let too_many = Planned::number("all", "carousel.item_count", "carousel", 21.0);
    assert!(matches!(
        catalog.check_at(&revision, &[too_many], TODAY),
        Err(Violation::AboveMax { .. })
    ));

    // The first failure returns, in the order given.
    let first_bad = [
        Planned::number("all", "text.length", "text", 10.0),
        Planned::number("all", "text.length", "text", 9999.0),
        Planned::number("all", "carousel.item_count", "carousel", 1.0),
    ];
    assert!(matches!(
        catalog.check_at(&revision, &first_bad, TODAY),
        Err(Violation::AboveMax { .. })
    ));
    assert_eq!(
        catalog.check_at(&revision, &[], TODAY),
        Ok(()),
        "nothing planned passes"
    );

    // Unit, type and absent rule.
    let wrong_unit = Planned::number("all", "text.length", "text", 10.0).in_unit("bytes");
    assert!(matches!(
        catalog.check_at(&revision, &[wrong_unit], TODAY),
        Err(Violation::UnitMismatch { planned_unit, .. }) if planned_unit == "bytes"
    ));
    let wrong_type = Planned::text("all", "text.length", "text", "ten");
    assert!(matches!(
        catalog.check_at(&revision, &[wrong_type], TODAY),
        Err(Violation::WrongType { .. })
    ));
    let missing = Planned::number("all", "no.such.capability", "text", 1.0);
    assert!(matches!(
        catalog.check_at(&revision, &[missing], TODAY),
        Err(Violation::NoRule { platform, .. }) if platform == "threads"
    ));

    // An unsourced rule blocks use: YouTube's verified-account duration states no maximum.
    let youtube = catalog.revision("youtube");
    let unsourced = Planned::number("verified_google_account", "video.duration", "video", 30.0)
        .in_unit("minutes");
    assert!(matches!(
        catalog.check_at(&youtube, &[unsourced], TODAY),
        Err(Violation::Blocked {
            status: Status::Unsourced,
            ..
        })
    ));

    // X: both account types are checked against their own limit.
    let x = catalog.revision("x");
    let long_ok = Planned::number("premium_or_verified", "video.duration", "video", 100.0);
    assert_eq!(catalog.check_at(&x, &[long_ok], TODAY), Ok(()));
    let long_bad = Planned::number("default", "video.duration", "video", 100.0);
    assert!(matches!(
        catalog.check_at(&x, &[long_bad], TODAY),
        Err(Violation::AboveMax { max, .. }) if max == 20.0
    ));

    // Enum and boolean rules.
    let instagram = catalog.revision("instagram");
    let manual = Planned::text("all", "trial_reel.graduation", "reel", "MANUAL");
    assert_eq!(catalog.check_at(&instagram, &[manual], TODAY), Ok(()));
    let auto = Planned::text("all", "trial_reel.graduation", "reel", "AUTO");
    match catalog.check_at(&instagram, &[auto], TODAY) {
        Err(Violation::NotAllowed {
            planned, allowed, ..
        }) => {
            assert_eq!(planned, "AUTO");
            assert!(allowed.iter().any(|a| a == "MANUAL"));
        }
        other => panic!("AUTO must not be allowed: {other:?}"),
    }
    let public_host = Planned::boolean("all", "media.public_host", "any", true);
    assert_eq!(catalog.check_at(&revision, &[public_host], TODAY), Ok(()));
    let private_host = Planned::boolean("all", "media.public_host", "any", false);
    assert!(matches!(
        catalog.check_at(&revision, &[private_host], TODAY),
        Err(Violation::NotExact { .. })
    ));

    // The typed accessor reads the sourced hashtag maximum; a platform without one is NoRule.
    assert_eq!(catalog.hashtag_max_at("instagram", "all", TODAY), Ok(30));
    assert!(matches!(
        catalog.hashtag_max_at("threads", "all", TODAY),
        Err(Violation::NoRule { .. })
    ));
}

fn revision_ids() {
    let catalog = Catalog::bundled();
    let revision = catalog.revision("threads");
    assert_eq!(revision.platform, "threads");
    assert_eq!(revision.hash.len(), 32);
    assert!(
        revision
            .hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "lowercase hex: {}",
        revision.hash
    );
    assert_eq!(
        revision,
        Catalog::bundled().revision("threads"),
        "stable across loads"
    );
    assert_ne!(
        revision.hash,
        catalog.revision("x").hash,
        "scoped to its platform"
    );
    assert!(catalog.is_current(&revision));

    let id = catalog.revision_id("threads");
    assert_eq!(id, revision.to_string());
    assert_eq!(id, format!("threads:{}", revision.hash));
    assert!(catalog.is_current_id(&id));
    assert_eq!(RuleRevision::parse(&id), Ok(revision.clone()));
    assert_eq!(id.parse::<RuleRevision>(), Ok(revision.clone()));
    assert!(!catalog.is_current_id("threads:00000000000000000000000000000000"));
    assert!(!catalog.is_current_id("not an id"));
    for bad in [
        "threads:ABC",
        "threads",
        ":00000000000000000000000000000000",
        "threads:0000000000000000000000000000000G",
        "threads:0000000000000000000000000000000",
        "threads:000000000000000000000000000000000",
        "threads:0000000000000000000000000000000A",
    ] {
        assert!(
            matches!(RuleRevision::parse(bad), Err(Violation::BadRevisionId { ref text }) if text == bad),
            "must refuse {bad:?}"
        );
    }

    let planned = Planned::number("all", "text.length", "text", 10.0).in_unit("characters");
    assert_eq!(
        catalog.check_id_at(&id, std::slice::from_ref(&planned), TODAY),
        Ok(())
    );
    assert!(matches!(
        catalog.check_id_at(
            "threads:00000000000000000000000000000000",
            &[planned.clone()],
            TODAY
        ),
        Err(Violation::StaleRevision { .. })
    ));
    assert!(matches!(
        catalog.check_id_at("garbage", &[planned], TODAY),
        Err(Violation::BadRevisionId { .. })
    ));

    // A platform with no records hashes the empty set, deterministically.
    let none = catalog.revision("no-such-platform");
    assert_eq!(none, catalog.revision("no-such-platform"));
    assert_eq!(Catalog::empty().revision("no-such-platform"), none);
    assert!(Catalog::empty().rules().is_empty());
    let nothing = Planned::number("all", "text.length", "text", 1.0);
    assert!(matches!(
        Catalog::empty().check_at(&Catalog::empty().revision("threads"), &[nothing], TODAY),
        Err(Violation::NoRule { .. })
    ));
}

fn merging() {
    let base = Catalog::bundled();
    let old = base.revision("threads");
    let rules_before = base.rules().len();

    let next = r#"{"ruleId":"threads.text.length","platform":"threads","accountType":"all","capability":"text.length","format":"text","limitKind":"max","max":600,"inclusive":true,"unit":"characters","sourceUrl":"https://example.test/new","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-20","ruleRevision":2,"supersedes":1,"status":"sourced"}"#;
    let merged = base
        .merge_json(&doc(&[next.to_owned()]))
        .expect("next revision merges");
    assert_eq!(base.revision("threads"), old, "merge returns a new catalog");
    assert_eq!(base.rules().len(), rules_before);
    assert!(
        !merged.is_current(&old),
        "work bound to the old revision is no longer current"
    );
    assert_ne!(merged.revision("threads"), old);
    assert_eq!(merged.rules().len(), rules_before + 1, "history is kept");
    assert_eq!(
        merged
            .get("threads", "all", "text.length", "text")
            .unwrap()
            .revision(),
        2
    );
    assert_eq!(
        merged.revision("x"),
        base.revision("x"),
        "other platforms are untouched"
    );
    let planned = Planned::number("all", "text.length", "text", 550.0);
    assert!(matches!(
        merged.check_at(&old, std::slice::from_ref(&planned), TODAY),
        Err(Violation::StaleRevision { .. })
    ));
    assert_eq!(
        merged.check_at(&merged.revision("threads"), &[planned], TODAY),
        Ok(())
    );

    let same_limit = r#"{"ruleId":"threads.text.length","platform":"threads","accountType":"all","capability":"text.length","format":"text","limitKind":"max","max":500,"inclusive":true,"unit":"characters","sourceUrl":"https://example.test/moved","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-20","ruleRevision":2,"supersedes":1,"status":"sourced"}"#;
    let moved = base.merge_json(&doc(&[same_limit.to_owned()])).unwrap();
    assert_eq!(
        moved.revision("threads"),
        base.revision("threads"),
        "a source-only change keeps the hash"
    );

    let edit = sourced(
        "threads.text.length",
        "threads",
        "text.length",
        "max",
        r#""max":900"#,
    );
    assert!(matches!(
        base.merge_json(&doc(&[edit])),
        Err(CatalogError::NotNextRevision {
            expected: 2,
            got: 1,
            ..
        })
    ));
    let skip = r#"{"ruleId":"threads.text.length","platform":"threads","accountType":"all","capability":"text.length","format":"text","limitKind":"max","max":900,"sourceUrl":"https://example.test/x","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-20","ruleRevision":3,"supersedes":2,"status":"sourced"}"#;
    assert!(matches!(
        base.merge_json(&doc(&[skip.to_owned()])),
        Err(CatalogError::NotNextRevision {
            expected: 2,
            got: 3,
            ..
        })
    ));
    let renamed = r#"{"ruleId":"threads.other","platform":"threads","accountType":"all","capability":"text.length","format":"text","limitKind":"max","max":900,"sourceUrl":"https://example.test/x","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-20","ruleRevision":2,"supersedes":1,"status":"sourced"}"#;
    assert!(matches!(
        base.merge_json(&doc(&[renamed.to_owned()])),
        Err(CatalogError::Invalid { .. })
    ));
    assert!(matches!(
        base.merge_json(r#"{"schema":2,"rules":[]}"#),
        Err(CatalogError::Schema(2))
    ));
    assert!(matches!(
        base.merge_json("not json"),
        Err(CatalogError::Json(_))
    ));

    let new_key = sourced(
        "bluesky.text.length",
        "bluesky",
        "text.length",
        "max",
        r#""max":300"#,
    );
    let with_new = base.merge_json(&doc(&[new_key])).unwrap();
    assert_eq!(
        with_new
            .get("bluesky", "all", "text.length", "text")
            .unwrap()
            .limit(),
        Limit::Max(300.0)
    );
    assert_eq!(
        with_new
            .get("bluesky", "all", "text.length", "text")
            .unwrap()
            .revision(),
        1,
        "a new key starts at revision 1"
    );

    // merge() takes another catalog directly.
    let overrides = Catalog::from_json(&doc(&[sourced(
        "bluesky.text.length",
        "bluesky",
        "text.length",
        "max",
        r#""max":300"#,
    )]))
    .unwrap();
    assert_eq!(
        base.merge(overrides).unwrap().rules().len(),
        rules_before + 1
    );
}

fn promotion_and_shape_rules() {
    let rule = |extra: &str| sourced("a.b", "a", "b", "max", extra);
    let invalid = |text: String, what: &str| {
        assert!(
            matches!(
                Catalog::from_json(&doc(&[text])),
                Err(CatalogError::Invalid { .. })
            ),
            "must refuse {what}"
        );
    };
    invalid(
        rule(r#""max":1"#).replace("official_fetched", "search_snippet"),
        "a sourced record from a search snippet",
    );
    invalid(
        rule(r#""max":1"#).replace(r#""sourceUrl":"https://example.test/doc","#, ""),
        "a sourced record without a source URL",
    );
    invalid(
        rule(r#""unit":"x""#),
        "a sourced record with no stated value",
    );
    invalid(
        rule(r#""max":1,"conflictNote":"sources disagree""#),
        "a sourced record with a conflict note",
    );
    invalid(
        rule(r#""max":1"#).replace("2026-10-08", "2026-13-40"),
        "an impossible fetch date",
    );
    invalid(
        rule(r#""max":1"#).replace("not_stated", "yesterday"),
        "a bad source date",
    );
    invalid(rule(r#""min":5,"max":1"#), "min above max");
    invalid(
        rule(r#""max":1"#).replace("\"ruleId\":\"a.b\"", "\"ruleId\":\" \""),
        "an empty rule id",
    );
    invalid(rule(r#""max":1,"sunsetDate":"soon""#), "a bad sunset date");
    invalid(rule(r#""max":1,"route":" ""#), "an empty route");
    invalid(
        rule(r#""max":1"#).replace("\"ruleRevision\":1", "\"ruleRevision\":0"),
        "revision 0",
    );
    invalid(
        rule(r#""max":1"#).replace("\"ruleRevision\":1", "\"ruleRevision\":1,\"supersedes\":1"),
        "supersedes at or above its own revision",
    );
    let unsourced_with_value = r#"{"ruleId":"a.b","platform":"a","accountType":"all","capability":"b","format":"text","limitKind":"max","max":95,"sourceGrade":"none","sourceDate":"not_stated","fetchedDate":"2026-10-08","ruleRevision":1,"status":"unsourced"}"#;
    invalid(
        unsourced_with_value.to_owned(),
        "an unsourced record that carries a number",
    );
    let stale_without_value = r#"{"ruleId":"a.b","platform":"a","accountType":"all","capability":"b","format":"text","limitKind":"max","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-08","ruleRevision":1,"status":"stale"}"#;
    invalid(
        stale_without_value.to_owned(),
        "a stale record without its last value",
    );

    assert!(matches!(
        Catalog::from_json("{"),
        Err(CatalogError::Json(_))
    ));
    assert!(matches!(
        Catalog::from_json(r#"{"schema":2,"rules":[]}"#),
        Err(CatalogError::Schema(2))
    ));
    assert!(matches!(
        Catalog::from_json(&rule(r#""max":1,"surprise":true"#)),
        Err(CatalogError::Json(_))
    ));
    let one = rule(r#""max":1"#);
    assert!(matches!(
        Catalog::from_json(&doc(&[one.clone(), one])),
        Err(CatalogError::DuplicateRevision { revision: 1, .. })
    ));
    let rev2_without_rev1 =
        rule(r#""max":1"#).replace("\"ruleRevision\":1", "\"ruleRevision\":2,\"supersedes\":1");
    assert!(matches!(
        Catalog::from_json(&doc(&[rev2_without_rev1])),
        Err(CatalogError::NotNextRevision {
            expected: 1,
            got: 2,
            ..
        })
    ));
    assert!(Catalog::from_json(r#"{"schema":1,"rules":[]}"#)
        .unwrap()
        .rules()
        .is_empty());
    for error in [
        CatalogError::Json("x".to_owned()),
        CatalogError::Schema(9),
        CatalogError::Invalid {
            rule_id: "a.b".to_owned(),
            reason: "why".to_owned(),
        },
        CatalogError::DuplicateRevision {
            rule_id: "a.b".to_owned(),
            revision: 1,
        },
        CatalogError::NotNextRevision {
            rule_id: "a.b".to_owned(),
            expected: 2,
            got: 1,
        },
    ] {
        is_error(&error);
        assert!(!error.to_string().is_empty(), "{error:?} has a message");
    }
}

fn limits_status_and_bounds() {
    let catalog = Catalog::from_json(&doc(&[
        sourced("t.min", "t", "min", "min", r#""min":3"#),
        sourced("t.max", "t", "max", "max", r#""max":7"#),
        sourced("t.range", "t", "range", "range", r#""min":2,"max":4"#),
        sourced("t.exact", "t", "exact", "exact", r#""value":42"#),
        sourced("t.label", "t", "label", "exact", r#""value":"v1""#),
        sourced("t.flag", "t", "flag", "boolean", r#""value":true"#),
        sourced("t.one", "t", "one", "enum", r#""value":"only""#),
        sourced("t.many", "t", "many", "enum", r#""value":["a","b"]"#),
        sourced(
            "t.excl",
            "t",
            "excl",
            "max",
            r#""max":10,"inclusive":false"#,
        ),
        sourced(
            "t.sunset",
            "t",
            "sunset",
            "exact",
            r#""value":"202510","sunsetDate":"2026-10-15""#,
        ),
    ]))
    .expect("limit fixture loads");
    let get = |capability: &str| {
        catalog
            .get("t", "all", capability, "text")
            .expect(capability)
    };
    assert_eq!(get("min").limit(), Limit::Min(3.0));
    assert_eq!(get("max").limit(), Limit::Max(7.0));
    assert_eq!(get("range").limit(), Limit::Range { min: 2.0, max: 4.0 });
    assert_eq!(get("exact").limit(), Limit::ExactNumber(42.0));
    assert_eq!(get("label").limit(), Limit::ExactText("v1".to_owned()));
    assert_eq!(get("flag").limit(), Limit::Boolean(true));
    assert_eq!(get("one").limit(), Limit::OneOf(vec!["only".to_owned()]));
    assert_eq!(
        get("many").limit(),
        Limit::OneOf(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(get("range").limit_kind, LimitKind::Range);
    assert_eq!(get("flag").limit_kind, LimitKind::Boolean);

    let revision = catalog.revision("t");
    let check = |planned: Planned| catalog.check_at(&revision, &[planned], TODAY);
    assert_eq!(check(Planned::number("all", "min", "text", 3.0)), Ok(()));
    assert!(matches!(
        check(Planned::number("all", "min", "text", 2.0)),
        Err(Violation::BelowMin { .. })
    ));
    assert_eq!(check(Planned::number("all", "max", "text", 7.0)), Ok(()));
    assert!(matches!(
        check(Planned::number("all", "max", "text", 8.0)),
        Err(Violation::AboveMax { .. })
    ));
    assert_eq!(check(Planned::number("all", "range", "text", 2.0)), Ok(()));
    assert_eq!(check(Planned::number("all", "range", "text", 4.0)), Ok(()));
    assert!(matches!(
        check(Planned::number("all", "range", "text", 1.0)),
        Err(Violation::BelowMin { .. })
    ));
    assert!(matches!(
        check(Planned::number("all", "range", "text", 5.0)),
        Err(Violation::AboveMax { .. })
    ));
    assert_eq!(check(Planned::number("all", "exact", "text", 42.0)), Ok(()));
    assert!(matches!(
        check(Planned::number("all", "exact", "text", 41.0)),
        Err(Violation::NotExact { .. })
    ));
    assert_eq!(check(Planned::text("all", "label", "text", "v1")), Ok(()));
    assert!(matches!(
        check(Planned::text("all", "label", "text", "v2")),
        Err(Violation::NotExact { .. })
    ));
    assert_eq!(check(Planned::boolean("all", "flag", "text", true)), Ok(()));
    assert!(matches!(
        check(Planned::boolean("all", "flag", "text", false)),
        Err(Violation::NotExact { .. })
    ));
    assert_eq!(check(Planned::text("all", "many", "text", "b")), Ok(()));
    assert!(matches!(
        check(Planned::text("all", "many", "text", "c")),
        Err(Violation::NotAllowed { .. })
    ));
    assert!(matches!(
        check(Planned::boolean("all", "max", "text", true)),
        Err(Violation::WrongType { .. })
    ));
    assert_eq!(
        Planned::number("all", "max", "text", 1.0).value,
        PlannedValue::Number(1.0)
    );
    assert_eq!(
        Planned::text("all", "many", "text", "a").value,
        PlannedValue::Text("a".to_owned())
    );
    assert_eq!(
        Planned::boolean("all", "flag", "text", true).value,
        PlannedValue::Bool(true)
    );

    // Bounds are inclusive unless the rule says otherwise.
    assert!(matches!(
        check(Planned::number("all", "excl", "text", 10.0)),
        Err(Violation::AboveMax { .. })
    ));
    assert_eq!(check(Planned::number("all", "excl", "text", 9.0)), Ok(()));

    // A sourced rule is stale the day after its sunset date.
    let sunset = get("sunset");
    assert_eq!(sunset.status_at("2026-10-15"), Status::Sourced);
    assert!(!sunset.blocks_use_at("2026-10-15"));
    assert_eq!(sunset.status_at("2026-10-16"), Status::Stale);
    assert!(sunset.blocks_use_at("2026-10-16"));
    let version = Planned::text("all", "sunset", "text", "202510");
    assert_eq!(
        catalog.check_at(&revision, std::slice::from_ref(&version), "2026-10-15"),
        Ok(())
    );
    assert!(matches!(
        catalog.check_at(&revision, &[version], "2026-10-16"),
        Err(Violation::Blocked {
            status: Status::Stale,
            ..
        })
    ));
    assert_eq!(
        get("min").status_at("2999-12-31"),
        Status::Sourced,
        "no sunset, never stale"
    );

    // Display names and messages.
    assert_eq!(Status::Sourced.to_string(), "sourced");
    assert_eq!(Status::Unsourced.to_string(), "unsourced");
    assert_eq!(Status::Stale.to_string(), "stale");
    let violation = Violation::AboveMax {
        rule_id: "t.max".to_owned(),
        planned: 8.0,
        max: 7.0,
    };
    is_error(&violation);
    assert!(violation.to_string().contains("t.max"));
    for violation in [
        Violation::NoRule {
            platform: "t".to_owned(),
            account_type: "all".to_owned(),
            capability: "c".to_owned(),
            format: "text".to_owned(),
        },
        Violation::BadRevisionId {
            text: "x".to_owned(),
        },
        Violation::WrongType {
            rule_id: "t.max".to_owned(),
        },
        Violation::Blocked {
            rule_id: "t.max".to_owned(),
            status: Status::Stale,
        },
    ] {
        assert!(
            !violation.to_string().is_empty(),
            "{violation:?} has a message"
        );
    }
}

fn link_placements() {
    assert_eq!(
        LINK_PLACEMENTS,
        ["caption", "first_comment", "link_sticker", "bio", "none"]
    );
    for (name, placement) in [
        ("caption", LinkPlacement::Caption),
        ("first_comment", LinkPlacement::FirstComment),
        ("link_sticker", LinkPlacement::LinkSticker),
        ("bio", LinkPlacement::Bio),
        ("none", LinkPlacement::NoLink),
    ] {
        assert_eq!(placement.as_str(), name);
        assert_eq!(LinkPlacement::parse(name), Some(placement));
    }
    assert_eq!(LinkPlacement::parse("carousel"), None);
    assert_eq!(LinkPlacement::parse(""), None);

    let text = r#"{"schema":1,"rules":[{"ruleId":"x.link.placement","platform":"x","accountType":"all","capability":"link.placement","format":"any","limitKind":"enum","value":["caption","none"],"sourceUrl":"https://example.test/doc","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-08","ruleRevision":1,"status":"sourced"}]}"#;
    let catalog = Catalog::from_json(text).unwrap();
    assert_eq!(
        catalog.link_placements_at("x", "all", TODAY),
        Ok(vec![LinkPlacement::Caption, LinkPlacement::NoLink])
    );
    assert!(matches!(
        catalog.link_placements_at("y", "all", TODAY),
        Err(Violation::NoRule { .. })
    ));
    let revision = catalog.revision("x");
    let planned = Planned::link_placement("all", LinkPlacement::Caption);
    assert_eq!(planned.capability, "link.placement");
    assert_eq!(planned.format, "any");
    assert_eq!(catalog.check_at(&revision, &[planned], TODAY), Ok(()));
    let refused = Planned::link_placement("all", LinkPlacement::Bio);
    assert!(matches!(
        catalog.check_at(&revision, &[refused], TODAY),
        Err(Violation::NotAllowed { .. })
    ));
    // A link.placement rule that names any other value is refused on load.
    let bad = text.replace("\"none\"]", "\"carousel\"]");
    assert!(matches!(
        Catalog::from_json(&bad),
        Err(CatalogError::Invalid { .. })
    ));
}

fn upload_routes() {
    let routed = sourced(
        "x.route.length",
        "x",
        "text.length",
        "max",
        r#""max":100,"unit":"characters","route":"native""#,
    );
    let catalog = Catalog::from_json(&doc(&[routed])).unwrap();
    assert!(
        catalog.get("x", "all", "text.length", "text").is_none(),
        "get() sees only unrestricted rules"
    );
    let rule = catalog
        .get_in("x", "all", "text.length", "text", "native")
        .expect("the route's own rule");
    assert_eq!(rule.route(), Some("native"));
    assert!(catalog
        .get_in("x", "all", "text.length", "text", "other")
        .is_none());

    let revision = catalog.revision("x");
    let over = Planned::number("all", "text.length", "text", 150.0).on_route("native");
    assert!(matches!(
        catalog.check_at(&revision, &[over], TODAY),
        Err(Violation::AboveMax { .. })
    ));
    let within = Planned::number("all", "text.length", "text", 50.0).on_route("native");
    assert_eq!(catalog.check_at(&revision, &[within], TODAY), Ok(()));
    let unrouted = Planned::number("all", "text.length", "text", 150.0);
    assert!(matches!(
        catalog.check_at(&revision, &[unrouted], TODAY),
        Err(Violation::NoRule { .. })
    ));

    // A route-specific rule is preferred over an unrestricted one.
    let both = Catalog::from_json(&doc(&[
        sourced(
            "x.len",
            "x",
            "text.length",
            "max",
            r#""max":280,"unit":"characters""#,
        ),
        sourced(
            "x.len.native",
            "x",
            "text.length",
            "max",
            r#""max":100,"unit":"characters","route":"native""#,
        ),
    ]))
    .unwrap();
    assert_eq!(
        both.get_in("x", "all", "text.length", "text", "native")
            .unwrap()
            .limit(),
        Limit::Max(100.0)
    );
    assert_eq!(
        both.get_in("x", "all", "text.length", "text", "elsewhere")
            .unwrap()
            .limit(),
        Limit::Max(280.0),
        "an unknown route falls back to the unrestricted rule"
    );
    assert_eq!(
        both.get("x", "all", "text.length", "text").unwrap().limit(),
        Limit::Max(280.0)
    );

    // with_route restricts a copy of a rule to one route.
    let plain: Rule = Catalog::bundled().rules()[0].clone();
    assert_eq!(plain.route(), None);
    assert_eq!(plain.clone().with_route("native").route(), Some("native"));
}

fn serialisation() {
    let catalog = Catalog::bundled();
    let json = catalog.to_json();
    let again = Catalog::from_json(&json).expect("to_json output loads");
    assert_eq!(again, catalog, "to_json / from_json is lossless");
    assert_eq!(again.revision("linkedin"), catalog.revision("linkedin"));
    assert_eq!(again.revision("instagram"), catalog.revision("instagram"));
    assert_eq!(again.to_json(), json, "serialisation is deterministic");

    let routed = Catalog::from_json(&doc(&[sourced(
        "x.route.length",
        "x",
        "text.length",
        "max",
        r#""max":100,"route":"native""#,
    )]))
    .unwrap();
    let back = Catalog::from_json(&routed.to_json()).unwrap();
    assert_eq!(back, routed, "a route survives the round trip");
    assert!(back
        .get_in("x", "all", "text.length", "text", "native")
        .is_some());
}

fn main() {
    bundled_catalog();
    checks_against_the_bundled_catalog();
    revision_ids();
    merging();
    promotion_and_shape_rules();
    limits_status_and_bounds();
    link_placements();
    upload_routes();
    serialisation();
    println!("OK rightkit-platform-rules (registry consumer)");
}
