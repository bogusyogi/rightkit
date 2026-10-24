// rightkit-channel-memory 0.2.1 consumed by exact registry pin (no features: the crate has none),
// outside the workspace.
//
// Proves, through the public API only, on one embedded channel-memory.v1 document (revision 2,
// four proofs: approved, collected, collected, withdrawn):
// - SCHEMA_SOURCE is the embedded schema (its $id); Decimal compares exact values (1240.00 equals
//   1240, -0 equals 0.0) and refuses malformed text, try_parse reports E_NUMBER_INVALID; Unit
//   accepts currency codes, built-in codes and open slugs, refuses the reserved _per_ forms and an
//   over-long base, try_parse reports E_UNIT_INVALID;
// - parse accepts the document and the typed model reads it back; refuses, in stage order, text
//   that is not JSON (Parse), another contract name (WrongContract), a missing, malformed or
//   unreadable version (MissingVersion, InvalidVersion, UnsupportedVersion for major 2 and for
//   minor 1), a schema defect (Schema, naming the path), and semantic defects (revision chain,
//   duplicate ids, reversed periods, claims that do not mirror their proof, two approved proofs
//   that disagree, E_PROOF_CONFLICT_SCOPE);
// - ensure_scope refuses another venture, channel or account (E_FOREIGN_SCOPE); approved_proofs
//   lists approved proofs only;
// - cite_proof returns the first failing reason code in contract order for every refusal (foreign
//   scope, observation, unknown proof, not approved, withdrawn, superseded, stale revision, not yet
//   valid, expired, claim not permitted, number, units, subject and period mismatch, open
//   conflict) and accepts an exact match, also with the number written without trailing zeros;
// - check_transition (legal moves, frozen approved content, terminal states, revision step),
//   check_successor, check_stale_arrival, check_supersession, conflicts_with;
// - check_write for each author and intent (base revision conflict, skill and daemon limits,
//   intent that does not match the status change, frozen content, lessons), validate_proof and
//   check_proof_write for a single record;
// - BannedMove::matches ignoring case; review_epoch and brief_reentry; ReasonCode wire names and
//   ProofStatus helpers.
// Does not prove: the daemon storage and sync this crate leaves to the daemon, the fixture files
// of the source repository, every optional field of the model, or the performance of the schema
// validators. Public API only.
// State: in-memory strings only. No network, no files, no process spawn, no user data.
use rightkit_channel_memory::{
    check_proof_write, check_stale_arrival, check_successor, check_supersession, check_transition,
    check_write, conflicts_with, parse, validate_proof, Approval, Author, BannedMove, BriefReentry,
    ChannelMemory, Citation, DateWindow, Decimal, Intent, MemoryError, ProofStatus, ReasonCode,
    Scope, Unit, Withdrawal, WriteRequest, SCHEMA_SOURCE,
};
use serde_json::{json, Value};

const TODAY: &str = "2026-10-11";
const LAUNCH: &str = "proof.receipt-launch-week";
const CLAIM: &str = "claim.launch-week-revenue";
const SUBJECT: &str = "subject.launch-week-revenue";
const SUCCESSOR: &str = "proof.receipt-launch-week-v2";
const USERS: &str = "proof.active-users";
const REFUND: &str = "proof.refund-rate-withdrawn";

fn main() {
    schema_and_scalars();
    parse_stages();
    scope_and_bank();
    citation_order();
    transitions_and_chain();
    controlled_writes();
    record_level_entry_points();
    banned_moves_and_epochs();
    println!("OK rightkit-channel-memory 0.2.1 (registry consumer)");
}

// ---- fixtures built in memory -------------------------------------------------------------

fn doc() -> Value {
    serde_json::from_str(DOC).expect("embedded document is JSON")
}

fn memory(value: &Value) -> ChannelMemory {
    parse(&value.to_string()).unwrap_or_else(|error| panic!("document must stay valid: {error}"))
}

fn head() -> ChannelMemory {
    memory(&doc())
}

fn proof_json<'a>(value: &'a mut Value, id: &str) -> &'a mut Value {
    value["proofs"]
        .as_array_mut()
        .expect("proofs")
        .iter_mut()
        .find(|item| item["id"] == id)
        .expect("proof in document")
}

fn scope(venture: &str, channel: &str, account: &str) -> Scope {
    Scope {
        venture: venture.into(),
        channel: channel.into(),
        account: account.into(),
    }
}

fn orthic() -> Scope {
    scope("orthic", "legion-notes", "main")
}

fn march() -> DateWindow {
    DateWindow {
        from: "2026-03-01".into(),
        to: "2026-03-07".into(),
    }
}

fn citation(value: &str) -> Citation {
    Citation {
        proof_id: LAUNCH.into(),
        proof_revision: 2,
        claim_id: CLAIM.into(),
        value: Decimal::parse(value).expect("decimal"),
        units: "USD".into(),
        subject: SUBJECT.into(),
        period: march(),
    }
}

/// Revision 3 of the document: the launch-week proof is superseded by the (now approved) v2 proof.
fn superseded_revision() -> Value {
    let mut next = doc();
    next["revision"] = json!(3);
    next["supersedes"] = json!(2);
    next["createdAt"] = json!("2026-10-09T12:00:00Z");
    let launch = proof_json(&mut next, LAUNCH);
    launch["revision"] = json!(3);
    launch["supersedesRevision"] = json!(2);
    launch["status"] = json!("superseded");
    launch["supersededBy"] = json!(SUCCESSOR);
    launch["permittedClaims"] = json!([]);
    launch["updatedAt"] = json!("2026-10-09T10:00:00Z");
    launch.as_object_mut().unwrap().remove("approval");
    let successor = proof_json(&mut next, SUCCESSOR);
    successor["revision"] = json!(2);
    successor["supersedesRevision"] = json!(1);
    successor["validFrom"] = json!("2026-10-09");
    successor["status"] = json!("approved");
    successor["approval"] = json!({ "approvedBy": "adrian", "approvedAt": "2026-10-09T10:00:00Z" });
    successor["provenance"]["authoredBy"] = json!("adrian");
    successor["updatedAt"] = json!("2026-10-09T10:00:00Z");
    next
}

fn next_revision() -> ChannelMemory {
    let mut next = doc();
    next["revision"] = json!(3);
    next["supersedes"] = json!(2);
    memory(&next)
}

fn proof_of<'a>(memory: &'a ChannelMemory, id: &str) -> &'a rightkit_channel_memory::Proof {
    memory
        .proofs
        .iter()
        .find(|item| item.id == id)
        .expect("proof in document")
}

fn refusal<T: std::fmt::Debug>(result: Result<T, MemoryError>) -> ReasonCode {
    match result {
        Err(MemoryError::Refused { code, .. }) => code,
        other => panic!("expected a typed refusal, got {other:?}"),
    }
}

fn request(author: Author, intent: Intent, base: u32, proposed: ChannelMemory) -> WriteRequest {
    WriteRequest {
        author,
        intent,
        base_revision: base,
        proposed,
    }
}

// ---- scalars -------------------------------------------------------------------------------

fn schema_and_scalars() {
    let schema: Value = serde_json::from_str(SCHEMA_SOURCE).expect("embedded schema is JSON");
    assert_eq!(
        schema["$id"], "urn:rightkit:contract:channel-memory:v1",
        "embedded schema identity"
    );

    let dec = |text: &str| Decimal::parse(text).unwrap_or_else(|| panic!("`{text}` must parse"));
    assert_eq!(dec("1240.00"), dec("1240"));
    assert_eq!(dec("-0"), dec("0.0"));
    assert_ne!(dec("1240.01"), dec("1240"));
    assert_ne!(dec("-1240"), dec("1240"));
    assert_eq!(
        dec("1240.00").as_str(),
        "1240.00",
        "text is kept as written"
    );
    assert_eq!(dec("1240.00").to_string(), "1240.00");
    for text in [
        "", "-", ".5", "1.", "1,240", "1e3", "01", "NaN", " 1", "1_0", "+1",
    ] {
        assert!(Decimal::parse(text).is_none(), "`{text}` must not parse");
    }
    assert!(
        Decimal::parse(&"9".repeat(41)).is_none(),
        "41 digits exceed the length limit"
    );
    assert_eq!(
        refusal(Decimal::try_parse("1,240")),
        ReasonCode::NumberInvalid,
        "typed construction reports E_NUMBER_INVALID"
    );
    assert!(Decimal::try_parse("1240.00").is_ok());
    assert_eq!(serde_json::to_string(&dec("12.50")).unwrap(), "\"12.50\"");
    assert!(
        serde_json::from_str::<Decimal>("12.5").is_err(),
        "a JSON number is not a decimal"
    );
    assert!(serde_json::from_str::<Decimal>("\"1,5\"").is_err());

    for text in [
        "USD",
        "EUR",
        "percent",
        "count",
        "days",
        "seconds",
        "views",
        "subscribers",
        "shorts_per_week",
        "orders_per_month",
        "posts_per_day",
        "clicks_per_year",
        "viewers_percent",
        "upper_percentile",
        "per_capita_score",
        "super_fans",
    ] {
        assert!(Unit::parse(text).is_some(), "`{text}` must be a unit");
    }
    let base_33 = format!("a{}", "b".repeat(32));
    for text in [
        "Views",
        "shorts per week",
        "9lives",
        "",
        "US",
        "USDX",
        "_per_week",
        "x_per_hour",
        "a_per_week_per_day",
        "views_per_",
        "views_per",
        "x_per",
        "x_per_per_day",
        "a_per_per_day",
        base_33.as_str(),
    ] {
        assert!(Unit::parse(text).is_none(), "`{text}` must be refused");
    }
    let base_32 = format!("a{}", "b".repeat(31));
    assert!(Unit::parse(&base_32).is_some());
    assert!(Unit::parse(&format!("{base_32}_per_day")).is_some());
    assert_eq!(Unit::parse("USD").unwrap().to_string(), "USD");
    assert_eq!(
        refusal(Unit::try_parse("x_per_hour")),
        ReasonCode::UnitInvalid
    );
    assert!(Unit::try_parse("shorts_per_week").is_ok());

    assert_eq!(
        ProofStatus::ALL.map(ProofStatus::as_str),
        ["collected", "approved", "withdrawn", "superseded"]
    );
    assert_eq!(
        ProofStatus::from_wire("approved"),
        Some(ProofStatus::Approved)
    );
    assert_eq!(
        ProofStatus::from_wire("pending"),
        None,
        "the retired status is unknown"
    );
    assert!(ProofStatus::Withdrawn.is_terminal());
    assert!(ProofStatus::Superseded.is_terminal());
    assert!(!ProofStatus::Approved.is_terminal());
    assert!(!ProofStatus::Collected.is_terminal());

    let codes = [
        (ReasonCode::ForeignScope, "E_FOREIGN_SCOPE"),
        (ReasonCode::UnknownProof, "E_UNKNOWN_PROOF"),
        (ReasonCode::ObservationAsFact, "E_OBSERVATION_AS_FACT"),
        (ReasonCode::ProofNotApproved, "E_PROOF_NOT_APPROVED"),
        (ReasonCode::ProofWithdrawn, "E_PROOF_WITHDRAWN"),
        (ReasonCode::ProofSuperseded, "E_PROOF_SUPERSEDED"),
        (ReasonCode::ProofRevisionStale, "E_PROOF_REVISION_STALE"),
        (ReasonCode::ProofExpired, "E_PROOF_EXPIRED"),
        (ReasonCode::ProofNotYetValid, "E_PROOF_NOT_YET_VALID"),
        (ReasonCode::ClaimNotPermitted, "E_CLAIM_NOT_PERMITTED"),
        (ReasonCode::NumberMismatch, "E_NUMBER_MISMATCH"),
        (ReasonCode::UnitMismatch, "E_UNIT_MISMATCH"),
        (ReasonCode::SubjectMismatch, "E_SUBJECT_MISMATCH"),
        (ReasonCode::PeriodMismatch, "E_PERIOD_MISMATCH"),
        (ReasonCode::ProofConflict, "E_PROOF_CONFLICT"),
        (ReasonCode::SkillStatusForbidden, "E_SKILL_STATUS_FORBIDDEN"),
        (ReasonCode::AuthorForbidden, "E_AUTHOR_FORBIDDEN"),
        (ReasonCode::IllegalTransition, "E_ILLEGAL_TRANSITION"),
        (ReasonCode::RevisionConflict, "revision_conflict"),
        (ReasonCode::NumberInvalid, "E_NUMBER_INVALID"),
        (ReasonCode::UnitInvalid, "E_UNIT_INVALID"),
        (ReasonCode::ProofConflictScope, "E_PROOF_CONFLICT_SCOPE"),
    ];
    for (code, wire) in codes {
        assert_eq!(code.as_str(), wire);
        assert_eq!(code.to_string(), wire);
    }
}

// ---- parse stages --------------------------------------------------------------------------

fn parse_stages() {
    let text = DOC;
    let memory = parse(text).expect("the embedded document is valid");
    assert_eq!(memory.contract, "channel-memory");
    assert_eq!(memory.memory_id, "memory.orthic-legion-notes-main");
    assert_eq!(memory.revision, 2);
    assert_eq!(memory.supersedes, Some(1));
    assert_eq!(memory.scope, orthic());
    assert_eq!(memory.proofs.len(), 4);
    assert_eq!(memory.observations.len(), 1);
    assert!(memory.conflicts.is_empty());
    let launch = proof_of(&memory, LAUNCH);
    assert_eq!(launch.status, ProofStatus::Approved);
    assert_eq!(launch.units.as_str(), "USD");
    assert_eq!(launch.value, Decimal::parse("1240").unwrap());
    assert_eq!(
        launch.permitted_claims[0].text,
        "Made $1,240 in launch week."
    );
    assert_eq!(launch.valid_until, None);
    assert_eq!(proof_of(&memory, REFUND).status, ProofStatus::Withdrawn);

    let back = serde_json::to_string(&memory).expect("serialise memory");
    assert_eq!(
        parse(&back).expect("serialised memory parses"),
        memory,
        "JSON round trip"
    );

    assert!(matches!(parse("{not json"), Err(MemoryError::Parse { .. })));

    let mut wrong = doc();
    wrong["contract"] = json!("content-brief");
    assert!(matches!(
        parse(&wrong.to_string()),
        Err(MemoryError::WrongContract { found }) if found == "content-brief"
    ));
    let mut missing = doc();
    missing.as_object_mut().unwrap().remove("contractVersion");
    assert_eq!(
        parse(&missing.to_string()),
        Err(MemoryError::MissingVersion)
    );
    for bad in ["1.0", "v1.0.0", "1.0.0.0", "01.0.0"] {
        let mut value = doc();
        value["contractVersion"] = json!(bad);
        assert!(
            matches!(
                parse(&value.to_string()),
                Err(MemoryError::InvalidVersion { .. })
            ),
            "version {bad:?}"
        );
    }
    for offered in ["2.0.0", "1.1.0", "0.9.0"] {
        let mut value = doc();
        value["contractVersion"] = json!(offered);
        assert!(
            matches!(
                parse(&value.to_string()),
                Err(MemoryError::UnsupportedVersion { .. })
            ),
            "version {offered:?}"
        );
    }
    let mut patch = doc();
    patch["contractVersion"] = json!("1.0.7");
    assert!(
        parse(&patch.to_string()).is_ok(),
        "patch numbers never gate acceptance"
    );

    let mut extra = doc();
    extra["unexpected"] = json!(true);
    match parse(&extra.to_string()) {
        Err(MemoryError::Schema { errors }) => assert!(!errors.is_empty()),
        other => panic!("expected Schema error, got {other:?}"),
    }
    let mut pending = doc();
    proof_json(&mut pending, USERS)["status"] = json!("pending");
    assert!(matches!(
        parse(&pending.to_string()),
        Err(MemoryError::Schema { .. })
    ));
    let mut number = doc();
    proof_json(&mut number, USERS)["value"] = json!(310);
    assert!(
        matches!(parse(&number.to_string()), Err(MemoryError::Schema { .. })),
        "a proof value is text, never a JSON number"
    );
    let mut unit = doc();
    proof_json(&mut unit, USERS)["units"] = json!("x_per_hour");
    assert!(matches!(
        parse(&unit.to_string()),
        Err(MemoryError::Schema { .. })
    ));
    let mut authoritative = doc();
    authoritative["observations"][0]["authoritative"] = json!(true);
    assert!(
        parse(&authoritative.to_string()).is_err(),
        "an observation can never be authoritative"
    );

    let semantic = |value: &Value| match parse(&value.to_string()) {
        Err(MemoryError::Semantic { rule }) => rule,
        other => panic!("expected Semantic error, got {other:?}"),
    };
    let mut chain = doc();
    chain["supersedes"] = json!(5);
    assert!(semantic(&chain).contains("revision 2"));
    let mut first = doc();
    first["revision"] = json!(1);
    assert!(
        matches!(parse(&first.to_string()), Err(MemoryError::Schema { .. })),
        "revision 1 may not carry a supersedes key"
    );
    let mut third = doc();
    third["revision"] = json!(3);
    assert!(
        semantic(&third).contains("must supersede revision 2"),
        "revision 3 must follow revision 2"
    );
    let mut duplicate = doc();
    let again = duplicate["proofs"][1].clone();
    duplicate["proofs"].as_array_mut().unwrap().push(again);
    assert!(semantic(&duplicate).contains("duplicate"));
    let mut reversed = doc();
    proof_json(&mut reversed, LAUNCH)["period"] =
        json!({ "from": "2026-03-07", "to": "2026-03-01" });
    assert!(semantic(&reversed).contains("period ends before it starts"));
    let mut window = doc();
    proof_json(&mut window, LAUNCH)["validUntil"] = json!("2026-10-01");
    assert!(semantic(&window).contains("validUntil is before validFrom"));
    let mut mirror = doc();
    proof_json(&mut mirror, LAUNCH)["permittedClaims"][0]["value"] = json!("1250.00");
    assert!(semantic(&mirror).contains("must mirror"));
    let mut dangling = doc();
    proof_json(&mut dangling, USERS)["observationRefs"] = json!(["obs.unknown"]);
    assert!(semantic(&dangling).contains("obs.unknown"));
    let mut clash = doc();
    {
        let other = proof_json(&mut clash, USERS);
        other["subject"] = json!(SUBJECT);
        other["units"] = json!("USD");
        other["period"] = json!({ "from": "2026-03-01", "to": "2026-03-07" });
        other["value"] = json!("999.00");
        other["status"] = json!("approved");
        other["approval"] = json!({ "approvedBy": "adrian", "approvedAt": "2026-10-07T18:00:00Z" });
    }
    let rule = semantic(&clash);
    assert!(rule.contains("E_PROOF_CONFLICT_SCOPE"), "{rule}");
}

// ---- scope and bank ------------------------------------------------------------------------

fn scope_and_bank() {
    let memory = head();
    assert!(memory.ensure_scope(&orthic()).is_ok());
    for foreign in [
        scope("orthic", "kids-cartoon", "main"),
        scope("other-venture", "legion-notes", "main"),
        scope("orthic", "legion-notes", "alt"),
    ] {
        assert_eq!(
            refusal(memory.ensure_scope(&foreign)),
            ReasonCode::ForeignScope
        );
    }
    let approved: Vec<&str> = memory
        .approved_proofs()
        .map(|proof| proof.id.as_str())
        .collect();
    assert_eq!(approved, [LAUNCH], "only approved proofs are facts");
    assert!(!memory.has_open_conflict(LAUNCH));
    assert_eq!(memory.open_conflicts(LAUNCH).count(), 0);
}

// ---- citation order ------------------------------------------------------------------------

fn citation_order() {
    let memory = head();
    let proof = memory
        .cite_proof(&orthic(), &citation("1240.00"), TODAY)
        .expect("exact approved citation");
    assert_eq!(proof.id, LAUNCH);
    assert!(
        memory
            .cite_proof(&orthic(), &citation("1240"), TODAY)
            .is_ok(),
        "the same exact value without trailing zeros is the same number"
    );

    let cite = |memory: &ChannelMemory, citation: &Citation, today: &str| {
        refusal(memory.cite_proof(&orthic(), citation, today))
    };
    assert_eq!(
        refusal(memory.cite_proof(
            &scope("orthic", "kids-cartoon", "main"),
            &citation("1240.00"),
            TODAY
        )),
        ReasonCode::ForeignScope
    );
    assert_eq!(
        cite(&memory, &citation("1420.00"), TODAY),
        ReasonCode::NumberMismatch
    );

    let mut other = citation("1240.00");
    other.units = "EUR".into();
    assert_eq!(cite(&memory, &other, TODAY), ReasonCode::UnitMismatch);
    let mut other = citation("1240.00");
    other.subject = "subject.other".into();
    assert_eq!(cite(&memory, &other, TODAY), ReasonCode::SubjectMismatch);
    let mut other = citation("1240.00");
    other.period.to = "2026-03-08".into();
    assert_eq!(cite(&memory, &other, TODAY), ReasonCode::PeriodMismatch);
    let mut other = citation("1240.00");
    other.claim_id = "claim.invented".into();
    assert_eq!(cite(&memory, &other, TODAY), ReasonCode::ClaimNotPermitted);
    let mut other = citation("1240.00");
    other.proof_revision = 1;
    assert_eq!(cite(&memory, &other, TODAY), ReasonCode::ProofRevisionStale);
    let mut other = citation("1240.00");
    other.proof_id = "proof.nowhere".into();
    assert_eq!(cite(&memory, &other, TODAY), ReasonCode::UnknownProof);
    let mut other = citation("1240.00");
    other.proof_id = "obs.pricing-page".into();
    assert_eq!(
        cite(&memory, &other, TODAY),
        ReasonCode::ObservationAsFact,
        "website text can never be cited as a fact"
    );
    assert_eq!(
        cite(&memory, &citation("1240.00"), "2026-10-06"),
        ReasonCode::ProofNotYetValid
    );

    let mut collected = citation("310");
    collected.proof_id = USERS.into();
    collected.proof_revision = 1;
    assert_eq!(
        cite(&memory, &collected, TODAY),
        ReasonCode::ProofNotApproved
    );
    let mut withdrawn = citation("2.5");
    withdrawn.proof_id = REFUND.into();
    withdrawn.proof_revision = 2;
    assert_eq!(cite(&memory, &withdrawn, TODAY), ReasonCode::ProofWithdrawn);

    let mut expiring = doc();
    proof_json(&mut expiring, LAUNCH)["validUntil"] = json!("2026-10-10");
    let expiring = self::memory(&expiring);
    assert_eq!(
        cite(&expiring, &citation("1240.00"), TODAY),
        ReasonCode::ProofExpired
    );
    assert!(
        expiring
            .cite_proof(&orthic(), &citation("1240.00"), "2026-10-10")
            .is_ok(),
        "the last valid day is inclusive"
    );

    let superseded = self::memory(&superseded_revision());
    assert_eq!(
        cite(&superseded, &citation("1240.00"), TODAY).as_str(),
        "E_PROOF_SUPERSEDED"
    );

    // Open conflict: two devices edited the approved proof from the same base revision.
    let mut conflicted = doc();
    let base = proof_json(&mut conflicted, LAUNCH).clone();
    let body = |status: &str| {
        let mut body = base.clone();
        body["revision"] = json!(3);
        body["supersedesRevision"] = json!(2);
        body["updatedAt"] = json!("2026-10-10T10:00:00Z");
        if status == "withdrawn" {
            body["status"] = json!("withdrawn");
            body["permittedClaims"] = json!([]);
            body.as_object_mut().unwrap().remove("approval");
            body["withdrawal"] = json!({
                "withdrawnBy": "adrian",
                "withdrawnAt": "2026-10-10T10:05:00Z",
                "reason": "Withdrawn on the second device."
            });
        }
        body
    };
    conflicted["conflicts"] = json!([{
        "id": "conflict.launch-week-1",
        "proofId": LAUNCH,
        "baseRevision": 2,
        "bodies": [
            { "device": "device-a", "proof": body("approved") },
            { "device": "device-b", "proof": body("withdrawn") }
        ],
        "status": "open"
    }]);
    let conflicted = self::memory(&conflicted);
    assert!(conflicted.has_open_conflict(LAUNCH));
    assert_eq!(conflicted.open_conflicts(LAUNCH).count(), 1);
    let record = &conflicted.conflicts[0];
    assert!(record.is_open());
    assert_eq!(record.status.as_str(), "open");
    assert!(record.chosen_body().is_none());
    assert_eq!(
        record.body_for("device-b").map(|b| b.proof.status),
        Some(ProofStatus::Withdrawn)
    );
    assert!(record.body_for("device-z").is_none());
    assert_eq!(
        cite(&conflicted, &citation("1240.00"), TODAY),
        ReasonCode::ProofConflict,
        "every claim on a proof in an open conflict is refused"
    );
}

// ---- transitions and the revision chain ----------------------------------------------------

fn transitions_and_chain() {
    let head = head();
    let next = memory(&superseded_revision());
    let launch = proof_of(&head, LAUNCH);
    let successor = proof_of(&head, SUCCESSOR);

    assert!(
        check_transition(launch, launch).is_ok(),
        "an unchanged proof passes"
    );
    assert!(
        check_transition(successor, proof_of(&next, SUCCESSOR)).is_ok(),
        "collected to approved"
    );
    assert!(
        check_transition(launch, proof_of(&next, LAUNCH)).is_ok(),
        "approved to superseded"
    );

    let mut frozen = launch.clone();
    frozen.revision = 3;
    frozen.supersedes_revision = Some(2);
    frozen.value = Decimal::parse("1250.00").unwrap();
    assert_eq!(
        refusal(check_transition(launch, &frozen)),
        ReasonCode::IllegalTransition,
        "approved content is frozen"
    );

    let refund = proof_of(&head, REFUND);
    let mut restored = refund.clone();
    restored.status = ProofStatus::Collected;
    restored.withdrawal = None;
    restored.revision = 3;
    restored.supersedes_revision = Some(2);
    assert_eq!(
        refusal(check_transition(refund, &restored)),
        ReasonCode::ProofWithdrawn,
        "withdrawn is terminal"
    );

    let mut back = proof_of(&next, LAUNCH).clone();
    let gone = back.clone();
    back.status = ProofStatus::Approved;
    back.superseded_by = None;
    back.revision = 4;
    back.supersedes_revision = Some(3);
    assert_eq!(
        refusal(check_transition(&gone, &back)),
        ReasonCode::ProofSuperseded,
        "superseded is terminal"
    );

    let mut jump = successor.clone();
    jump.status = ProofStatus::Superseded;
    jump.superseded_by = Some(LAUNCH.into());
    jump.revision = 2;
    jump.supersedes_revision = Some(1);
    assert_eq!(
        refusal(check_transition(successor, &jump)),
        ReasonCode::IllegalTransition,
        "collected cannot jump to superseded"
    );

    let mut skip = launch.clone();
    skip.status = ProofStatus::Withdrawn;
    skip.approval = None;
    skip.withdrawal = Some(Withdrawal {
        withdrawn_by: "adrian".into(),
        withdrawn_at: "2026-10-12T09:00:00Z".into(),
        reason: "Test withdrawal".into(),
    });
    skip.revision = 5;
    skip.supersedes_revision = Some(2);
    assert_eq!(
        refusal(check_transition(launch, &skip)),
        ReasonCode::IllegalTransition,
        "revision must advance by one"
    );

    assert!(check_successor(&head, &next).is_ok());
    let mut late = next.clone();
    late.revision = 4;
    assert_eq!(
        refusal(check_successor(&head, &late)),
        ReasonCode::RevisionConflict
    );
    let mut other = next.clone();
    other.memory_id = "memory.other".into();
    assert!(matches!(
        check_successor(&head, &other),
        Err(MemoryError::Semantic { .. })
    ));
    let mut removed = next.clone();
    removed.proofs.retain(|proof| proof.id != USERS);
    assert_eq!(
        refusal(check_successor(&head, &removed)),
        ReasonCode::IllegalTransition,
        "a proof is withdrawn, never removed"
    );
    let mut fresh = next_revision();
    let mut approved_new = proof_of(&fresh, SUCCESSOR).clone();
    approved_new.id = "proof.fresh-approved".into();
    approved_new.status = ProofStatus::Approved;
    approved_new.approval = Some(Approval {
        approved_by: "adrian".into(),
        approved_at: "2026-10-12T09:00:00Z".into(),
    });
    fresh.proofs.push(approved_new);
    assert_eq!(
        refusal(check_successor(&head, &fresh)),
        ReasonCode::IllegalTransition,
        "a new proof starts collected"
    );

    // A stale arrival may not restore a withdrawn proof, and a different body loses to the head.
    let mut stale = head.clone();
    stale.revision = 1;
    stale.supersedes = None;
    assert!(
        check_stale_arrival(&head, &stale).is_ok(),
        "an older copy of the same bodies is accepted"
    );
    {
        let refund = stale.proofs.iter_mut().find(|p| p.id == REFUND).unwrap();
        refund.status = ProofStatus::Approved;
        refund.withdrawal = None;
        refund.approval = Some(approved("2026-02-10T09:00:00Z"));
    }
    assert_eq!(
        refusal(check_stale_arrival(&head, &stale)),
        ReasonCode::ProofWithdrawn
    );
    let mut differs = head.clone();
    differs.revision = 1;
    differs.supersedes = None;
    differs
        .proofs
        .iter_mut()
        .find(|p| p.id == USERS)
        .unwrap()
        .source_excerpt = "edited elsewhere".into();
    assert_eq!(
        refusal(check_stale_arrival(&head, &differs)),
        ReasonCode::RevisionConflict
    );
    assert!(
        matches!(
            check_stale_arrival(&head, &head),
            Err(MemoryError::Semantic { .. })
        ),
        "a stale copy must be older"
    );

    assert!(check_supersession(proof_of(&next, LAUNCH), proof_of(&next, SUCCESSOR)).is_ok());
    assert_eq!(
        refusal(check_supersession(launch, successor)),
        ReasonCode::IllegalTransition,
        "an approved proof is not superseded"
    );
    assert_eq!(
        refusal(check_supersession(proof_of(&next, LAUNCH), successor)),
        ReasonCode::ProofNotApproved,
        "the successor must be approved"
    );

    // conflicts_with: the same subject, period and units with a different exact value.
    let mut twin = launch.clone();
    twin.id = "proof.twin".into();
    assert!(
        !conflicts_with(launch, &twin),
        "equal values do not conflict"
    );
    twin.value = Decimal::parse("1240.000").unwrap();
    assert!(!conflicts_with(launch, &twin), "1240.000 equals 1240.00");
    twin.value = Decimal::parse("999").unwrap();
    assert!(conflicts_with(launch, &twin));
    twin.units = Unit::parse("EUR").unwrap();
    assert!(!conflicts_with(launch, &twin), "other units never conflict");
    assert!(
        !conflicts_with(launch, launch),
        "a proof never conflicts with itself"
    );
    twin.units = launch.units.clone();
    twin.status = ProofStatus::Collected;
    assert!(
        !conflicts_with(launch, &twin),
        "only approved proofs conflict"
    );
}

fn approved(at: &str) -> Approval {
    Approval {
        approved_by: "adrian".into(),
        approved_at: at.into(),
    }
}

// ---- controlled writes ---------------------------------------------------------------------

fn controlled_writes() {
    let head = head();
    let next = memory(&superseded_revision());

    assert!(check_write(
        &head,
        &request(Author::Adrian, Intent::Supersede, 2, next.clone())
    )
    .is_ok());
    assert_eq!(
        refusal(check_write(
            &head,
            &request(Author::Adrian, Intent::Supersede, 1, next.clone())
        )),
        ReasonCode::RevisionConflict,
        "a write names the base revision it was made against"
    );
    assert_eq!(
        refusal(check_write(
            &head,
            &request(Author::Skill, Intent::Approve, 2, next.clone())
        )),
        ReasonCode::SkillStatusForbidden,
        "a skill cannot approve"
    );
    assert_eq!(
        refusal(check_write(
            &head,
            &request(Author::Skill, Intent::Collect, 2, next.clone())
        )),
        ReasonCode::SkillStatusForbidden,
        "relabelling a status change as a collection does not help"
    );
    assert_eq!(
        refusal(check_write(
            &head,
            &request(Author::Daemon, Intent::Approve, 2, next.clone())
        )),
        ReasonCode::AuthorForbidden,
        "the daemon cannot approve either"
    );
    assert_eq!(
        refusal(check_write(
            &head,
            &request(Author::Adrian, Intent::Withdraw, 2, next.clone())
        )),
        ReasonCode::IllegalTransition,
        "the intent must match the status changes in the write"
    );

    let mut retitled = doc();
    retitled["revision"] = json!(3);
    retitled["supersedes"] = json!(2);
    retitled["voice"]["tone"] = json!("warm and loud");
    assert_eq!(
        refusal(check_write(
            &head,
            &request(Author::Skill, Intent::Collect, 2, memory(&retitled))
        )),
        ReasonCode::AuthorForbidden,
        "a skill cannot change authoritative memory"
    );
    assert!(
        check_write(
            &head,
            &request(Author::Adrian, Intent::Edit, 2, memory(&retitled))
        )
        .is_ok(),
        "Adrian may"
    );

    let mut collected = next_revision();
    let mut fresh = proof_of(&collected, SUCCESSOR).clone();
    fresh.id = "proof.interview-collected".into();
    fresh.permitted_claims.clear();
    collected.proofs.push(fresh);
    assert!(check_write(
        &head,
        &request(Author::Skill, Intent::Collect, 2, collected)
    )
    .is_ok());

    let mut observed = doc();
    observed["revision"] = json!(3);
    observed["supersedes"] = json!(2);
    observed["observations"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "obs.review-page", "origin": "website", "text": "Customers call it fast.",
            "observedAt": "2026-10-10T10:00:00Z", "authoritative": false,
            "source": { "kind": "website", "ref": "https://example.test/reviews" }
        }));
    assert!(check_write(
        &head,
        &request(Author::Skill, Intent::Observe, 2, memory(&observed))
    )
    .is_ok());
    assert_eq!(
        refusal(check_write(
            &head,
            &request(Author::Skill, Intent::Conflict, 2, memory(&observed))
        )),
        ReasonCode::AuthorForbidden,
        "a skill cannot record a conflict"
    );
    assert!(check_write(
        &head,
        &request(Author::Daemon, Intent::Conflict, 2, memory(&observed))
    )
    .is_ok());

    let mut lesson = doc();
    lesson["revision"] = json!(3);
    lesson["supersedes"] = json!(2);
    lesson["lessons"].as_array_mut().unwrap().push(json!({
        "id": "lesson.statement-hooks",
        "text": "Statement hooks lost viewers faster in this set.",
        "recordedAt": "2026-10-10T10:00:00Z",
        "source": { "kind": "analytics_export", "ref": "exports/tiktok-retention-2026-10" }
    }));
    for author in [Author::Skill, Author::Daemon, Author::Adrian] {
        assert!(
            check_write(&head, &request(author, Intent::Lesson, 2, memory(&lesson))).is_ok(),
            "{author:?} may record a lesson"
        );
    }
    assert_eq!(
        refusal(check_write(
            &head,
            &request(Author::Skill, Intent::Lesson, 2, memory(&retitled))
        )),
        ReasonCode::AuthorForbidden,
        "a lesson write cannot carry other authoritative changes"
    );

    let wire = serde_json::to_string(&request(Author::Adrian, Intent::Approve, 2, next))
        .expect("serialise request");
    assert!(
        wire.contains("\"author\":\"adrian\"")
            && wire.contains("\"intent\":\"approve\"")
            && wire.contains("\"baseRevision\":2")
    );
    let mut forged: Value = serde_json::from_str(&wire).unwrap();
    forged["proposed"]["observations"][0]["authoritative"] = json!(true);
    assert!(
        serde_json::from_value::<WriteRequest>(forged).is_err(),
        "an observation cannot be proposed as authoritative"
    );
}

// ---- record-level entry points -------------------------------------------------------------

fn record_level_entry_points() {
    let head = head();
    let launch = proof_of(&head, LAUNCH);
    assert!(validate_proof(launch, &orthic()).is_ok());
    assert!(
        matches!(
            validate_proof(launch, &scope("Orthic", "legion-notes", "main")),
            Err(MemoryError::Schema { .. })
        ),
        "the scope argument is shape-checked"
    );
    let mut reversed = launch.clone();
    reversed.period = DateWindow {
        from: "2026-03-07".into(),
        to: "2026-03-01".into(),
    };
    let message = validate_proof(&reversed, &orthic())
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("period ends before it starts"),
        "{message}"
    );
    let mut mismatch = launch.clone();
    mismatch.permitted_claims[0].value = Decimal::parse("1.00").unwrap();
    assert!(
        validate_proof(&mismatch, &orthic()).is_err(),
        "claims must mirror the proof"
    );

    let mut withdrawn = launch.clone();
    withdrawn.status = ProofStatus::Withdrawn;
    withdrawn.approval = None;
    withdrawn.withdrawal = Some(Withdrawal {
        withdrawn_by: "adrian".into(),
        withdrawn_at: "2026-10-12T09:00:00Z".into(),
        reason: "Test withdrawal".into(),
    });
    withdrawn.permitted_claims.clear();
    withdrawn.revision = 3;
    withdrawn.supersedes_revision = Some(2);
    assert!(check_proof_write(Some(launch), &withdrawn, Author::Adrian).is_ok());
    assert_eq!(
        refusal(check_proof_write(Some(launch), &withdrawn, Author::Skill)),
        ReasonCode::SkillStatusForbidden
    );
    assert_eq!(
        refusal(check_proof_write(Some(launch), &withdrawn, Author::Daemon)),
        ReasonCode::AuthorForbidden
    );
    assert_eq!(
        refusal(check_proof_write(None, launch, Author::Adrian)),
        ReasonCode::IllegalTransition,
        "a new proof starts collected at revision 1"
    );
    let users = proof_of(&head, USERS);
    assert!(
        check_proof_write(None, users, Author::Skill).is_ok(),
        "a skill may collect a new proof"
    );
}

// ---- banned moves and review epochs --------------------------------------------------------

fn banned_moves_and_epochs() {
    let head = head();
    let hype = &head.banned_moves[0];
    assert_eq!(hype.id, "ban.hype");
    assert!(hype.matches("This one is a GAME-CHANGER for agents"));
    assert!(hype.matches("time to Unlock The Power of review"));
    assert!(!hype.matches("a plain hook about receipts"));
    let none = BannedMove {
        phrases: Vec::new(),
        ..hype.clone()
    };
    assert!(
        !none.matches("anything at all"),
        "a move without phrases matches nothing"
    );

    let epoch = head.review_epoch(&[SUCCESSOR, LAUNCH, LAUNCH, "proof.nowhere"]);
    assert_eq!(
        epoch,
        format!("proof.nowhere@missing;{LAUNCH}@2:approved;{SUCCESSOR}@1:collected"),
        "sorted by id, deduplicated, missing ids spelled out"
    );
    assert_eq!(
        head.brief_reentry(&[LAUNCH], &head.review_epoch(&[LAUNCH])),
        BriefReentry::Current
    );
    let next = memory(&superseded_revision());
    match next.brief_reentry(
        &[LAUNCH, SUCCESSOR],
        &head.review_epoch(&[LAUNCH, SUCCESSOR]),
    ) {
        BriefReentry::NeedsReview { changed, epoch } => {
            assert_eq!(changed, [LAUNCH, SUCCESSOR]);
            assert_eq!(epoch, next.review_epoch(&[LAUNCH, SUCCESSOR]));
        }
        BriefReentry::Current => panic!("revised facts send the brief back to review"),
    }
}

// The embedded document: channel-memory.v1, revision 2 (supersedes 1), scope orthic/legion-notes/main.
const DOC: &str = r####"
{
  "contract": "channel-memory",
  "contractVersion": "1.0.0",
  "memoryId": "memory.orthic-legion-notes-main",
  "revision": 2,
  "supersedes": 1,
  "createdAt": "2026-10-08T12:00:00Z",
  "scope": {
    "venture": "orthic",
    "channel": "legion-notes",
    "account": "main"
  },
  "brandCode": "orthic",
  "voice": {
    "tone": "direct, dry, concrete",
    "audience": "developers who run agents daily"
  },
  "visualSystem": {
    "style": "clean screen capture with a dark frame",
    "layout": "16:9 with lower-third title",
    "captionStyle": "small mono captions",
    "pacing": "one idea per cut",
    "sound": "no music, narrator only"
  },
  "pillars": [
    {
      "id": "pillar.workflow",
      "name": "Workflow",
      "description": "How a real session got unstuck."
    }
  ],
  "cadence": {
    "postsPerDay": 1,
    "timezone": "Europe/London",
    "batch": "daytime"
  },
  "guidance": {
    "doList": [
      "Show the receipt on screen."
    ],
    "dontList": [
      "Do not invent a metric."
    ]
  },
  "bannedMoves": [
    {
      "id": "ban.hype",
      "description": "No hype adjectives in hooks.",
      "appliesTo": "any",
      "phrases": [
        "game-changer",
        "unlock the power"
      ]
    }
  ],
  "bannedPhrases": [
    "game-changer",
    "unlock the power"
  ],
  "licenceScope": {
    "monetised": false,
    "assetRights": "licensed"
  },
  "proofs": [
    {
      "id": "proof.receipt-launch-week",
      "revision": 2,
      "supersedesRevision": 1,
      "subject": "subject.launch-week-revenue",
      "value": "1240.00",
      "units": "USD",
      "period": {
        "from": "2026-03-01",
        "to": "2026-03-07"
      },
      "observedAt": "2026-03-08T09:00:00Z",
      "validFrom": "2026-10-07",
      "validUntil": null,
      "sourceExcerpt": "Weekly export total: 1,240.00 USD",
      "source": {
        "kind": "connected_account_export",
        "ref": "exports/payments-2026-03.csv"
      },
      "observationRefs": [],
      "status": "approved",
      "approval": {
        "approvedBy": "adrian",
        "approvedAt": "2026-10-07T18:00:00Z"
      },
      "permittedClaims": [
        {
          "claimId": "claim.launch-week-revenue",
          "text": "Made $1,240 in launch week.",
          "value": "1240.00",
          "units": "USD",
          "subject": "subject.launch-week-revenue",
          "period": {
            "from": "2026-03-01",
            "to": "2026-03-07"
          }
        }
      ],
      "provenance": {
        "authoredBy": "adrian",
        "sourceRecordIds": []
      },
      "updatedAt": "2026-10-07T18:00:00Z"
    },
    {
      "id": "proof.receipt-launch-week-v2",
      "revision": 1,
      "supersedesRevision": null,
      "subject": "subject.launch-week-revenue",
      "value": "1260.00",
      "units": "USD",
      "period": {
        "from": "2026-03-01",
        "to": "2026-03-07"
      },
      "observedAt": "2026-10-08T09:00:00Z",
      "validFrom": "2026-10-08",
      "validUntil": null,
      "sourceExcerpt": "Corrected weekly export total: 1,260.00 USD",
      "source": {
        "kind": "connected_account_export",
        "ref": "exports/payments-2026-03-corrected.csv"
      },
      "observationRefs": [],
      "status": "collected",
      "permittedClaims": [
        {
          "claimId": "claim.launch-week-revenue-v2",
          "text": "Made $1,260 in launch week.",
          "value": "1260.00",
          "units": "USD",
          "subject": "subject.launch-week-revenue",
          "period": {
            "from": "2026-03-01",
            "to": "2026-03-07"
          }
        }
      ],
      "provenance": {
        "authoredBy": "skill",
        "sourceRecordIds": [
          "interview.2026-10-08"
        ]
      },
      "updatedAt": "2026-10-08T09:00:00Z"
    },
    {
      "id": "proof.active-users",
      "revision": 1,
      "supersedesRevision": null,
      "subject": "subject.active-users",
      "value": "310",
      "units": "count",
      "period": {
        "from": "2026-09-01",
        "to": "2026-09-30"
      },
      "observedAt": "2026-10-05T12:00:00Z",
      "validFrom": "2026-10-05",
      "validUntil": null,
      "sourceExcerpt": "Interview note, 5 October",
      "source": {
        "kind": "interview",
        "ref": "interviews/2026-10-05"
      },
      "observationRefs": [],
      "status": "collected",
      "permittedClaims": [],
      "provenance": {
        "authoredBy": "skill",
        "sourceRecordIds": [
          "interview.2026-10-05"
        ]
      },
      "updatedAt": "2026-10-05T12:00:00Z"
    },
    {
      "id": "proof.refund-rate-withdrawn",
      "revision": 2,
      "supersedesRevision": 1,
      "subject": "subject.refund-rate",
      "value": "2.5",
      "units": "percent",
      "period": {
        "from": "2026-02-01",
        "to": "2026-02-28"
      },
      "observedAt": "2026-02-28T12:00:00Z",
      "validFrom": "2026-02-01",
      "validUntil": "2026-02-28",
      "sourceExcerpt": "Refund rate from the February export",
      "source": {
        "kind": "adrian_correction",
        "ref": "corrections/2026-10-06"
      },
      "observationRefs": [],
      "status": "withdrawn",
      "withdrawal": {
        "withdrawnBy": "adrian",
        "withdrawnAt": "2026-10-06T09:00:00Z",
        "reason": "Export row was a duplicate."
      },
      "permittedClaims": [],
      "provenance": {
        "authoredBy": "adrian",
        "sourceRecordIds": []
      },
      "updatedAt": "2026-10-06T09:00:00Z"
    }
  ],
  "observations": [
    {
      "id": "obs.pricing-page",
      "origin": "website",
      "text": "Pricing page reads 9 dollars per month.",
      "observedAt": "2026-10-07T10:00:00Z",
      "authoritative": false,
      "source": {
        "kind": "website",
        "ref": "https://example.test/pricing"
      }
    }
  ],
  "lessons": [
    {
      "id": "lesson.question-hooks",
      "text": "Question hooks held 30-second retention better in this set.",
      "recordedAt": "2026-10-06T08:00:00Z",
      "source": {
        "kind": "analytics_export",
        "ref": "exports/tiktok-retention-2026-10"
      }
    }
  ],
  "performanceNotes": [
    {
      "id": "perf.retention-question-hooks",
      "platform": "tiktok",
      "metric": "retention_30s",
      "window": {
        "from": "2026-09-20",
        "to": "2026-10-04"
      },
      "sampleSize": 14,
      "summary": "Median 30-second retention 41 percent for question hooks, 33 percent for statements.",
      "caveat": "Fourteen posts from one account; directional only.",
      "source": {
        "kind": "analytics_export",
        "ref": "exports/tiktok-retention-2026-10"
      }
    }
  ],
  "references": [],
  "conflicts": [],
  "provenance": {
    "authoredBy": "adrian",
    "sources": [
      {
        "kind": "adrian_correction",
        "ref": "corrections/2026-10-06"
      },
      {
        "kind": "connected_account",
        "ref": "exports/payments-2026-03.csv"
      }
    ]
  }
}
"####;
