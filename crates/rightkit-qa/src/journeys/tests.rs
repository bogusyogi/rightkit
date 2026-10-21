//! Tests for the journeys module: manifest refusals, the adapter round trip, and one test per
//! way a receipt must never be accepted.
use super::adapter::{
    journey_from_outcomes, journey_from_units, JourneyResult, Record, Reporter, SelectedJourney,
    Selection, StartInfo, UnitOutcome,
};
use super::manifest::{check, check_manifest, check_mapping, parse_manifest, parse_mapping};
use super::receipt::*;
use super::{
    code, is_rfc3339, ArtifactDigest, Evidence, EvidenceKind, JourneyStatus, Units, RECEIPT_SCHEMA,
};
use crate::harness::Outcome;
use crate::util::{new_id, sha256_hex};
use std::fs;
use std::path::PathBuf;

fn rep(c: char, n: usize) -> String {
    c.to_string().repeat(n)
}

const NONCE_CHAR: char = 'a';

struct Fx {
    dir: PathBuf,
    receipt: Receipt,
    results: Vec<u8>,
    manifest: super::Manifest,
    manifest_sha: String,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "rkqa-journeys-{name}-{}-{}",
        std::process::id(),
        new_id()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn manifest_text(ids: &[&str]) -> String {
    let mut t = String::from(
        "version = 1\napp = \"demo\"\ncoverage_owners = [\"owner\"]\n\n[[adapter]]\nname = \"demo-adapter\"\nbuild = [\"rightkit\", \"cargo\", \"test\", \"--no-run\", \"--message-format=json\"]\nexecutable = { role = \"adapter\", cargo_target = \"journeys_adapter\" }\nartifacts = [{ role = \"engine\", cargo_package = \"demo-engine\" }]\n",
    );
    for id in ids {
        t.push_str(&format!(
            "\n[[journey]]\nid = \"{id}\"\ntitle = \"Journey {id}\"\nplatforms = [\"windows\"]\nsurface = \"engine\"\nrunner = \"adapter\"\nadapter = \"demo-adapter\"\nentry = \"tests/j.scenario.toml\"\ntier = \"fast\"\ndata = \"generated\"\nrelease_critical = true\n"
        ));
    }
    t
}

fn passed(id: &str) -> JourneyResult {
    journey_from_outcomes(id, &[Outcome::Passed]).with_timing("2026-10-21T10:00:00Z", 5)
}

fn isolation() -> super::adapter::Isolation {
    super::adapter::Isolation {
        data_dir: "/tmp/rightkit-qa/demo-1".into(),
        guard: "passed".into(),
        qa_mode: "runtime".into(),
    }
}

fn start_info() -> StartInfo {
    StartInfo {
        adapter: "demo-adapter".into(),
        adapter_version: "1.0.0".into(),
        artifacts: vec![ArtifactDigest {
            role: "engine".into(),
            path: "/build/engine".into(),
            sha256: rep('e', 64),
        }],
        isolation: isolation(),
    }
}

/// Run the real adapter helper for `written`, then derive a receipt from what it wrote.
fn build(selected: &[&str], written: Vec<JourneyResult>) -> Fx {
    let dir = temp_dir("fx");
    fs::create_dir_all(dir.join("tests")).unwrap();
    fs::write(dir.join("tests/j.scenario.toml"), "name = \"x\"\n").unwrap();
    let text = manifest_text(selected);
    let (manifest, manifest_sha) = parse_manifest(text.as_bytes()).unwrap();
    let sel = Selection {
        protocol: 1,
        run_id: "run-1".into(),
        nonce: rep(NONCE_CHAR, 32),
        app: "demo".into(),
        platform: "windows".into(),
        tier: "fast".into(),
        manifest_sha256: manifest_sha.clone(),
        journeys: selected
            .iter()
            .map(|id| SelectedJourney {
                id: id.to_string(),
                entry: "tests/j.scenario.toml".into(),
            })
            .collect(),
    };
    let results_path = dir.join("results.jsonl");
    fs::write(&results_path, b"").unwrap();
    let mut r = Reporter::open(&sel, &results_path).unwrap();
    r.start(start_info()).unwrap();
    for w in &written {
        r.journey(w).unwrap();
    }
    let _ = r.end();
    let results = fs::read(&results_path).unwrap();

    let (records, errs) = parse_results(&results);
    assert!(errs.is_empty(), "{errs:?}");
    let mut journeys = vec![];
    for id in selected {
        let rec = records.iter().find_map(|x| match x {
            Record::Journey(j) if j.id == *id => Some(j),
            _ => None,
        });
        journeys.push(match rec {
            Some(j) => ReceiptJourney {
                id: j.id.clone(),
                status: j.status,
                release_critical: true,
                started_at: j.started_at.clone(),
                duration_ms: j.duration_ms,
                units: j.units,
                evidence: j.evidence.clone(),
                reason: j.reason.clone(),
            },
            None => ReceiptJourney {
                id: id.to_string(),
                status: JourneyStatus::Missing,
                release_critical: true,
                started_at: "1970-01-01T00:00:00Z".into(),
                duration_ms: 0,
                units: Units::default(),
                evidence: vec![],
                reason: Some("no result".into()),
            },
        });
    }
    let tree = rep('d', 40);
    let receipt = Receipt {
        schema: RECEIPT_SCHEMA.into(),
        run_id: "run-1".into(),
        nonce: rep(NONCE_CHAR, 32),
        app: "demo".into(),
        kind: ReceiptKind::Functional,
        verdict: Verdict::Passed,
        started_at: "2026-10-21T10:00:00Z".into(),
        ended_at: "2026-10-21T10:05:00Z".into(),
        host: Host {
            os: "windows".into(),
            arch: "x86_64".into(),
            session: "interactive".into(),
        },
        source: Source {
            head: rep('c', 40),
            tree: tree.clone(),
            dirty: false,
            diff_sha256: None,
        },
        manifest: FileRef {
            path: "journeys.toml".into(),
            sha256: manifest_sha.clone(),
        },
        mapping: None,
        build: Build {
            command: vec!["rightkit".into(), "cargo".into(), "test".into()],
            source_tree: tree.clone(),
            tree_after_build: tree,
            started_at: "2026-10-21T10:00:00Z".into(),
            ended_at: "2026-10-21T10:01:00Z".into(),
            broker_receipt: None,
            artifacts: vec![
                BuildArtifact {
                    role: "adapter".into(),
                    path: "/build/adapter".into(),
                    size: 10,
                    sha256: rep('b', 64),
                    produced_by: ProducedBy::BuildOutput,
                },
                BuildArtifact {
                    role: "engine".into(),
                    path: "/build/engine".into(),
                    size: 20,
                    sha256: rep('e', 64),
                    produced_by: ProducedBy::BuildOutput,
                },
            ],
        },
        launched: vec![
            Launched {
                role: "adapter".into(),
                path: "/build/adapter".into(),
                size: 10,
                sha256: rep('b', 64),
            },
            Launched {
                role: "engine".into(),
                path: "/build/engine".into(),
                size: 20,
                sha256: rep('e', 64),
            },
        ],
        isolation: ReceiptIsolation {
            data_dir: "/tmp/rightkit-qa/demo-1".into(),
            guard: "passed".into(),
            qa_mode: "runtime".into(),
            real_data_touched: false,
        },
        adapter: AdapterRun {
            name: "demo-adapter".into(),
            version: "1.0.0".into(),
            protocol: 1,
            argv: vec![],
            exit_code: 0,
            results_sha256: sha256_hex(&results),
        },
        selection: ReceiptSelection {
            ids: selected.iter().map(|s| s.to_string()).collect(),
            platform: "windows".into(),
            tier: "fast".into(),
        },
        journeys,
        counts: Counts::default(),
        problems: vec![],
    };
    let mut fx = Fx {
        dir,
        receipt,
        results,
        manifest,
        manifest_sha,
    };
    let v = run(&fx);
    fx.receipt.counts = v.counts;
    let v = run(&fx);
    fx.receipt.verdict = v.verdict;
    fx
}

fn ok() -> Fx {
    build(&["J-001", "J-002"], vec![passed("J-001"), passed("J-002")])
}

fn opts(fx: &Fx) -> ValidateOptions<'_> {
    ValidateOptions {
        expected_tree: Some(fx.receipt.source.tree.as_str()),
        manifest: Some((&fx.manifest, fx.manifest_sha.as_str())),
        results: Some(fx.results.as_slice()),
        evidence_root: Some(fx.dir.as_path()),
        allow_dirty: false,
    }
}

fn run(fx: &Fx) -> Validation {
    validate_receipt(&fx.receipt, &opts(fx))
}

fn set_results(fx: &mut Fx, bytes: Vec<u8>) {
    fx.receipt.adapter.results_sha256 = sha256_hex(&bytes);
    fx.results = bytes;
}

fn text(fx: &Fx) -> String {
    String::from_utf8(fx.results.clone()).unwrap()
}

fn refused(v: &Validation, c: &str) {
    assert!(!v.passed(), "must not pass; problems {:?}", v.problems);
    assert_ne!(v.verdict, Verdict::Passed);
    assert!(
        v.problems.iter().any(|p| p.code == c),
        "expected {c}, got {:?}",
        v.problems
    );
}

// ---- the good path and the adapter round trip -------------------------------------------------

#[test]
fn a_complete_run_validates_and_nothing_else_does() {
    let fx = ok();
    let v = run(&fx);
    assert!(v.passed(), "{:?}", v.problems);
    assert_eq!(v.verdict, Verdict::Passed);
    assert_eq!(
        v.counts,
        Counts {
            selected: 2,
            executed: 2,
            passed: 2,
            ..Counts::default()
        }
    );
    assert!(v.unchecked.is_empty(), "{:?}", v.unchecked);
}

#[test]
fn adapter_lines_round_trip_through_the_validator() {
    let fx = ok();
    let t = text(&fx);
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines.len(), 4, "start, 2 journeys, end");
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["type"], "start");
    assert_eq!(first["protocol"], 1);
    assert_eq!(first["run_id"], "run-1");
    assert_eq!(first["nonce"], rep(NONCE_CHAR, 32));
    assert_eq!(first["isolation"]["guard"], "passed");
    let j: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(j["type"], "journey");
    assert_eq!(j["id"], "J-001");
    assert_eq!(j["status"], "passed");
    assert!(j["reason"].is_null());
    let last: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
    assert_eq!(last["type"], "end");
    assert_eq!(last["selected"], 2);
    assert_eq!(last["reported"], 2);
    let (records, errs) = parse_results(&fx.results);
    assert!(errs.is_empty());
    assert!(matches!(records.first(), Some(Record::Start(_))));
    assert!(matches!(records.last(), Some(Record::End(_))));
    assert!(run(&fx).passed());
}

#[test]
fn selection_parse_refuses_zero_and_malformed() {
    let good = |journeys: &str, nonce: &str| {
        format!(
            "{{\"protocol\":1,\"run_id\":\"r\",\"nonce\":\"{nonce}\",\"app\":\"demo\",\"platform\":\"windows\",\"tier\":\"fast\",\"manifest_sha256\":\"{}\",\"journeys\":{journeys}}}",
            rep('1', 64)
        )
    };
    let one = "[{\"id\":\"J-001\",\"entry\":\"e\"}]";
    assert!(Selection::parse(good(one, &rep('a', 32)).as_bytes()).is_ok());
    assert!(
        Selection::parse(good("[]", &rep('a', 32)).as_bytes()).is_err(),
        "zero selected"
    );
    assert!(
        Selection::parse(good(one, "short").as_bytes()).is_err(),
        "bad nonce"
    );
    let dup = "[{\"id\":\"J-001\",\"entry\":\"e\"},{\"id\":\"J-001\",\"entry\":\"e\"}]";
    assert!(
        Selection::parse(good(dup, &rep('a', 32)).as_bytes()).is_err(),
        "duplicate id"
    );
    let v2 = good(one, &rep('a', 32)).replace("\"protocol\":1", "\"protocol\":2");
    assert!(Selection::parse(v2.as_bytes()).is_err(), "wrong protocol");
}

#[test]
fn the_reporter_refuses_what_the_protocol_forbids() {
    let sel = |ids: &[&str]| Selection {
        protocol: 1,
        run_id: "r".into(),
        nonce: rep('a', 32),
        app: "demo".into(),
        platform: "windows".into(),
        tier: "fast".into(),
        manifest_sha256: rep('1', 64),
        journeys: ids
            .iter()
            .map(|i| SelectedJourney {
                id: i.to_string(),
                entry: "e".into(),
            })
            .collect(),
    };
    let dir = temp_dir("rep");
    let path = dir.join("results.jsonl");
    fs::write(&path, b"").unwrap();
    let s = sel(&["J-001", "J-002"]);
    let mut r = Reporter::open(&s, &path).unwrap();
    assert!(r.journey(&passed("J-001")).is_err(), "journey before start");
    assert!(r.end().is_err(), "end before start");
    let mut bad = start_info();
    bad.isolation.guard = "failed".into();
    assert!(r.start(bad).is_err(), "isolation not passed");
    r.start(start_info()).unwrap();
    assert!(r.start(start_info()).is_err(), "second start");
    assert!(r.journey(&passed("J-999")).is_err(), "unselected journey");
    let mut zero = passed("J-001");
    zero.units = Units::default();
    assert!(r.journey(&zero).is_err(), "passed with zero units");
    let mut skipped_unit = passed("J-001");
    skipped_unit.units = Units {
        total: 2,
        passed: 1,
        failed: 0,
        skipped: 1,
    };
    assert!(
        r.journey(&skipped_unit).is_err(),
        "passed with a skipped unit"
    );
    let mut missing = passed("J-001");
    missing.status = JourneyStatus::Missing;
    assert!(r.journey(&missing).is_err(), "adapter never writes missing");
    let mut no_reason = journey_from_units("J-001", &[UnitOutcome::Failed(String::new())]);
    no_reason.reason = None;
    assert!(
        r.journey(&no_reason).is_err(),
        "reason required unless passed"
    );
    r.journey(&passed("J-001")).unwrap();
    assert!(r.journey(&passed("J-001")).is_err(), "duplicate journey");
    let end = r.end();
    assert!(end.is_err(), "J-002 never reported, end must say so");
    assert!(r.journey(&passed("J-002")).is_err(), "journey after end");
    let t = fs::read_to_string(&path).unwrap();
    let last: serde_json::Value = serde_json::from_str(t.lines().last().unwrap()).unwrap();
    assert_eq!(last["selected"], 2);
    assert_eq!(last["reported"], 1, "end states the true counts");
    // a stale file is refused
    assert!(
        Reporter::open(&s, &path).is_err(),
        "results file already holds data"
    );
    assert!(
        Reporter::open(&s, &dir.join("nope.jsonl")).is_err(),
        "never created by the adapter"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn outcomes_map_one_to_one() {
    let p = journey_from_outcomes("J-1", &[Outcome::Passed, Outcome::Passed]);
    assert_eq!(p.status, JourneyStatus::Passed);
    assert_eq!(
        p.units,
        Units {
            total: 2,
            passed: 2,
            failed: 0,
            skipped: 0
        }
    );
    let s = journey_from_outcomes(
        "J-1",
        &[Outcome::Passed, Outcome::Skipped("needs mic".into())],
    );
    assert_eq!(s.status, JourneyStatus::Skipped);
    assert_eq!(s.reason.as_deref(), Some("needs mic"));
    let none = journey_from_outcomes("J-1", &[]);
    assert_eq!(
        none.status,
        JourneyStatus::Errored,
        "a group that matched nothing is errored"
    );
    let f = journey_from_units(
        "J-1",
        &[
            UnitOutcome::Skipped("s".into()),
            UnitOutcome::Failed("boom".into()),
        ],
    );
    assert_eq!(f.status, JourneyStatus::Failed, "failed outranks skipped");
    assert_eq!(f.reason.as_deref(), Some("boom"));
}

// ---- receipt refusals --------------------------------------------------------------------------

#[test]
fn zero_selected_is_never_a_pass() {
    let mut fx = ok();
    fx.receipt.selection.ids.clear();
    fx.receipt.journeys.clear();
    fx.receipt.counts = Counts::default();
    let v = run(&fx);
    refused(&v, code::ZERO_EXECUTED);
    assert_eq!(v.verdict, Verdict::Incomplete);
}

#[test]
fn zero_executed_is_never_a_pass() {
    let mut fx = build(&["J-001"], vec![]);
    let v = run(&fx);
    refused(&v, code::ZERO_EXECUTED);
    refused(&v, code::MISSING_RESULT);
    assert_eq!(v.verdict, Verdict::Incomplete);
    // even a receipt that claims "passed" with a hand-edited count does not pass
    fx.receipt.verdict = Verdict::Passed;
    fx.receipt.counts.executed = 1;
    fx.receipt.counts.passed = 1;
    fx.receipt.counts.missing = 0;
    let v = run(&fx);
    refused(&v, code::COUNTS_MISMATCH);
    refused(&v, code::VERDICT_MISMATCH);
}

#[test]
fn a_misspelled_filter_that_runs_nothing_is_errored_not_passed() {
    let fx = build(
        &["J-001"],
        vec![journey_from_outcomes("J-001", &[]).with_timing("2026-10-21T10:00:00Z", 1)],
    );
    let v = run(&fx);
    refused(&v, code::JOURNEY_FAILED);
    assert_eq!(v.counts.errored, 1);
}

#[test]
fn a_missing_result_is_refused() {
    let mut fx = ok();
    fx.receipt.journeys.retain(|j| j.id != "J-002");
    refused(&run(&fx), code::MISSING_RESULT);
    let fx = build(&["J-001", "J-002"], vec![passed("J-001")]);
    let v = run(&fx);
    refused(&v, code::MISSING_RESULT);
    assert_eq!(v.counts.missing, 1);
}

#[test]
fn a_duplicate_result_is_refused() {
    let mut fx = ok();
    let dup = fx.receipt.journeys[0].clone();
    fx.receipt.journeys.push(dup);
    refused(&run(&fx), code::DUPLICATE_RESULT);
    // a duplicate dropped from the receipt but visible in the adapter's file
    let mut fx = ok();
    let line = text(&fx).lines().nth(1).unwrap().to_string();
    let mut t = text(&fx);
    t = t.replacen(&line, &format!("{line}\n{line}"), 1);
    set_results(&mut fx, t.into_bytes());
    refused(&run(&fx), code::DUPLICATE_RESULT);
}

#[test]
fn an_unselected_result_is_refused() {
    let mut fx = ok();
    let mut extra = fx.receipt.journeys[0].clone();
    extra.id = "J-999".into();
    fx.receipt.journeys.push(extra);
    refused(&run(&fx), code::UNSELECTED_RESULT);
}

#[test]
fn skipped_failed_and_errored_are_not_passes() {
    let skipped = JourneyResult {
        status: JourneyStatus::Skipped,
        units: Units {
            total: 1,
            passed: 0,
            failed: 0,
            skipped: 1,
        },
        reason: Some("not today".into()),
        ..passed("J-001")
    };
    let v = run(&build(&["J-001"], vec![skipped]));
    refused(&v, code::SKIPPED);
    let failed = journey_from_units("J-001", &[UnitOutcome::Failed("boom".into())]);
    refused(&run(&build(&["J-001"], vec![failed])), code::JOURNEY_FAILED);
    let errored = JourneyResult {
        status: JourneyStatus::Errored,
        reason: Some("adapter crashed".into()),
        ..passed("J-001")
    };
    refused(
        &run(&build(&["J-001"], vec![errored])),
        code::JOURNEY_FAILED,
    );
}

#[test]
fn a_passed_journey_without_passing_units_is_refused() {
    let mut fx = ok();
    fx.receipt.journeys[0].units = Units::default();
    refused(&run(&fx), code::EMPTY_JOURNEY);
}

#[test]
fn a_stale_nonce_or_run_id_discards_the_results() {
    let mut fx = ok();
    let t = text(&fx).replacen(&rep(NONCE_CHAR, 32), &rep('b', 32), 1);
    set_results(&mut fx, t.into_bytes());
    refused(&run(&fx), code::STALE_RESULTS);

    let mut fx = ok();
    let t = text(&fx).replacen("\"run_id\":\"run-1\"", "\"run_id\":\"run-0\"", 1);
    set_results(&mut fx, t.into_bytes());
    refused(&run(&fx), code::STALE_RESULTS);

    let mut fx = ok();
    fx.receipt.nonce = rep('c', 32);
    refused(&run(&fx), code::STALE_RESULTS);
}

#[test]
fn a_truncated_or_reordered_results_file_is_a_protocol_violation() {
    let mut fx = ok();
    let last_nl = text(&fx).trim_end().rfind('\n').unwrap();
    let t = text(&fx)[..=last_nl].to_string(); // end record cut off
    set_results(&mut fx, t.into_bytes());
    refused(&run(&fx), code::PROTOCOL_VIOLATION);

    let mut fx = ok();
    let t = text(&fx).replacen("\"selected\":2", "\"selected\":1", 1);
    set_results(&mut fx, t.into_bytes());
    refused(&run(&fx), code::PROTOCOL_VIOLATION);

    let mut fx = ok();
    set_results(&mut fx, b"not json\n".to_vec());
    refused(&run(&fx), code::PROTOCOL_VIOLATION);

    let mut fx = ok();
    set_results(&mut fx, Vec::new());
    refused(&run(&fx), code::PROTOCOL_VIOLATION);
}

#[test]
fn a_results_digest_mismatch_is_refused() {
    let mut fx = ok();
    fx.receipt.adapter.results_sha256 = rep('0', 64);
    refused(&run(&fx), code::ADAPTER_EXIT);
}

#[test]
fn a_receipt_that_disagrees_with_the_adapters_records_is_refused() {
    let mut fx = ok();
    fx.receipt.journeys[0].duration_ms += 1;
    refused(&run(&fx), code::RECEIPT_RESULTS_MISMATCH);
}

#[test]
fn without_the_results_file_nothing_passes() {
    let fx = ok();
    let v = validate_receipt(
        &fx.receipt,
        &ValidateOptions {
            results: None,
            ..opts(&fx)
        },
    );
    refused(&v, code::RESULTS_UNVERIFIED);
    assert_eq!(v.verdict, Verdict::Incomplete);
}

#[test]
fn artifact_digest_bindings_are_enforced() {
    // launched binary is not the built one
    let mut fx = ok();
    fx.receipt.launched[1].sha256 = rep('f', 64);
    refused(&run(&fx), code::ARTIFACT_NOT_FROM_BUILD);
    // launched role the build never produced
    let mut fx = ok();
    fx.receipt.launched[1].role = "ghost".into();
    refused(&run(&fx), code::ARTIFACT_NOT_FROM_BUILD);
    // nothing built
    let mut fx = ok();
    fx.receipt.build.artifacts.clear();
    refused(&run(&fx), code::ARTIFACT_NOT_FROM_BUILD);
    // the adapter reported another digest than the runner measured
    let mut fx = ok();
    let t = text(&fx).replacen(&rep('e', 64), &rep('f', 64), 1);
    set_results(&mut fx, t.into_bytes());
    refused(&run(&fx), code::ARTIFACT_MISMATCH);
    // a launched binary the adapter never reported (other than its own executable)
    let mut fx = ok();
    fx.receipt.launched.push(Launched {
        role: "extra".into(),
        path: "/x".into(),
        size: 1,
        sha256: rep('9', 64),
    });
    fx.receipt.build.artifacts.push(BuildArtifact {
        role: "extra".into(),
        path: "/x".into(),
        size: 1,
        sha256: rep('9', 64),
        produced_by: ProducedBy::BuildOutput,
    });
    refused(&run(&fx), code::ARTIFACT_MISMATCH);
    // nothing launched
    let mut fx = ok();
    fx.receipt.launched.clear();
    refused(&run(&fx), code::NO_LAUNCH);
}

#[test]
fn source_and_manifest_bindings_are_enforced() {
    let mut fx = ok();
    fx.receipt.build.tree_after_build = rep('9', 40);
    refused(&run(&fx), code::SOURCE_MISMATCH);

    let fx = ok();
    let other_tree = rep('9', 40);
    let v = validate_receipt(
        &fx.receipt,
        &ValidateOptions {
            expected_tree: Some(other_tree.as_str()),
            ..opts(&fx)
        },
    );
    refused(&v, code::SOURCE_MISMATCH);

    let mut fx = ok();
    fx.manifest_sha = rep('7', 64);
    refused(&run(&fx), code::SOURCE_MISMATCH);

    let mut fx = ok();
    fx.manifest.journey[0].platforms = vec!["macos".into()];
    refused(&run(&fx), code::SOURCE_MISMATCH);

    let mut fx = ok();
    fx.receipt.app = "other".into();
    refused(&run(&fx), code::SOURCE_MISMATCH);

    let mut fx = ok();
    fx.receipt.build.artifacts[1].produced_by = ProducedBy::BrokerReceipt;
    refused(&run(&fx), code::SOURCE_MISMATCH);
}

#[test]
fn a_dirty_tree_is_refused_unless_allowed() {
    let mut fx = ok();
    fx.receipt.source.dirty = true;
    fx.receipt.source.diff_sha256 = Some(rep('4', 64));
    refused(&run(&fx), code::DIRTY_TREE);
    let v = validate_receipt(
        &fx.receipt,
        &ValidateOptions {
            allow_dirty: true,
            ..opts(&fx)
        },
    );
    assert!(!v.problems.iter().any(|p| p.code == code::DIRTY_TREE));
    fx.receipt.source.diff_sha256 = None;
    refused(&run(&fx), code::SOURCE_MISMATCH);
}

#[test]
fn an_isolation_failure_is_refused() {
    let mut fx = ok();
    fx.receipt.isolation.guard = "failed".into();
    refused(&run(&fx), code::ISOLATION);
    let mut fx = ok();
    fx.receipt.isolation.real_data_touched = true;
    refused(&run(&fx), code::ISOLATION);
    let mut fx = ok();
    fx.receipt.isolation.qa_mode = "release".into();
    refused(&run(&fx), code::ISOLATION);
    let mut fx = ok();
    fx.receipt.isolation.data_dir.clear();
    refused(&run(&fx), code::ISOLATION);
    // the adapter's own start record says the guard did not pass
    let mut fx = ok();
    let t = text(&fx).replacen("\"guard\":\"passed\"", "\"guard\":\"failed\"", 1);
    set_results(&mut fx, t.into_bytes());
    refused(&run(&fx), code::ISOLATION);
}

#[test]
fn a_non_zero_adapter_exit_is_refused_even_when_every_record_passed() {
    let mut fx = ok();
    fx.receipt.adapter.exit_code = 1;
    let v = run(&fx);
    refused(&v, code::ADAPTER_EXIT);
    assert_eq!(
        v.counts.passed, 2,
        "records are all passed, exit code still decides"
    );
}

#[test]
fn a_forged_verdict_is_not_believed() {
    let mut fx = ok();
    fx.receipt.journeys[0].status = JourneyStatus::Failed;
    fx.receipt.journeys[0].reason = Some("x".into());
    fx.receipt.verdict = Verdict::Passed;
    let v = run(&fx);
    refused(&v, code::VERDICT_MISMATCH);
    // and the reverse: a failed claim over a clean run is inconsistent, not accepted
    let mut fx = ok();
    fx.receipt.verdict = Verdict::Failed;
    refused(&run(&fx), code::VERDICT_MISMATCH);
    // a receipt that lists its own problems cannot claim passed
    let mut fx = ok();
    fx.receipt
        .problems
        .push(super::Problem::new(code::SKIPPED, None, "x"));
    refused(&run(&fx), code::VERDICT_MISMATCH);
}

#[test]
fn wrong_schema_host_and_formats_are_refused() {
    let mut fx = ok();
    fx.receipt.schema = "rightkit.journeys.receipt/2".into();
    let v = run(&fx);
    refused(&v, code::SCHEMA);
    let mut fx = ok();
    fx.receipt.nonce = "short".into();
    refused(&run(&fx), code::SCHEMA);
    let mut fx = ok();
    fx.receipt.host.os = "windows".into();
    fx.receipt.selection.platform = "macos".into();
    refused(&run(&fx), code::HOST_MISMATCH);
    assert!(parse_receipt(b"{}").is_err());
    assert!(parse_receipt(b"not json").is_err());
    let mut fx = ok();
    fx.receipt.started_at = "yesterday".into();
    refused(&run(&fx), code::SCHEMA);
}

#[test]
fn evidence_hashes_are_checked_against_files() {
    let dir_file = |fx: &Fx| fx.dir.join("ev").join("log.txt");
    let mut fx = ok();
    fs::create_dir_all(fx.dir.join("ev")).unwrap();
    fs::write(dir_file(&fx), b"log line").unwrap();
    let sha = sha256_hex(b"log line");
    let evidence = vec![Evidence {
        path: "ev/log.txt".into(),
        sha256: sha,
        kind: EvidenceKind::Log,
    }];
    // write the record through the helper so the results file agrees with the receipt
    let mut fx2 = build(
        &["J-001"],
        vec![passed("J-001").with_evidence(evidence.clone())],
    );
    fs::create_dir_all(fx2.dir.join("ev")).unwrap();
    assert_eq!(
        run(&fx2)
            .problems
            .iter()
            .filter(|p| p.code == code::EVIDENCE_MISMATCH)
            .count(),
        1,
        "file absent"
    );
    fs::write(fx2.dir.join("ev/log.txt"), b"log line").unwrap();
    fx2.receipt.verdict = Verdict::Passed;
    assert!(run(&fx2).passed(), "{:?}", run(&fx2).problems);
    fs::write(fx2.dir.join("ev/log.txt"), b"tampered").unwrap();
    refused(&run(&fx2), code::EVIDENCE_MISMATCH);
    // path traversal is refused, never read
    fx.receipt.journeys[0].evidence = vec![Evidence {
        path: "../secret".into(),
        sha256: rep('1', 64),
        kind: EvidenceKind::Log,
    }];
    refused(&run(&fx), code::EVIDENCE_MISMATCH);
}

#[test]
fn rfc3339_shape() {
    assert!(is_rfc3339("2026-10-21T10:00:00Z"));
    assert!(is_rfc3339("2026-10-21T10:00:00.123+02:00"));
    assert!(!is_rfc3339("2026-10-21 10:00:00Z"));
    assert!(!is_rfc3339("2026-13-21T10:00:00Z"));
    assert!(!is_rfc3339("2026-10-21T10:00:00"));
    assert!(!is_rfc3339(""));
}

// ---- manifest and mapping ---------------------------------------------------------------------

fn manifest_issues(extra_edit: impl Fn(String) -> String) -> Vec<String> {
    let dir = temp_dir("man");
    fs::create_dir_all(dir.join("tests")).unwrap();
    fs::write(dir.join("tests/j.scenario.toml"), "").unwrap();
    let text = extra_edit(manifest_text(&["J-001"]));
    let (m, _) = parse_manifest(text.as_bytes()).unwrap();
    let issues = check_manifest(&m, &dir);
    let _ = fs::remove_dir_all(&dir);
    issues
}

fn has_issue(issues: &[String], needle: &str) -> bool {
    issues.iter().any(|i| i.contains(needle))
}

#[test]
fn a_good_manifest_has_no_issues() {
    assert!(manifest_issues(|t| t).is_empty());
}

#[test]
fn manifest_refusals() {
    assert!(has_issue(
        &manifest_issues(|t| t.replace("generated", "real")),
        "data = \"real\""
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("version = 1", "version = 2")),
        "version"
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("\"windows\"", "\"beos\"")),
        "platform"
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("\"fast\"", "\"sometimes\"")),
        "tier"
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("\"engine\"", "\"nowhere\"")),
        "surface"
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("adapter = \"demo-adapter\"", "adapter = \"ghost\"")),
        "not declared"
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("adapter = \"demo-adapter\"\nentry", "entry")),
        "requires adapter"
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("tests/j.scenario.toml", "tests/missing.scenario.toml")),
        "does not exist"
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("tests/j.scenario.toml", "../outside.toml")),
        "relative path inside"
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("tests/j.scenario.toml", "tests/nodir/*.toml")),
        "no directory"
    ));
    assert!(has_issue(
        &manifest_issues(|t| t.replace("platforms = [\"windows\"]", "platforms = []")),
        "platforms is empty"
    ));
    let dup = |t: String| {
        let j = t[t.find("[[journey]]").unwrap()..].to_string();
        format!("{t}{j}")
    };
    assert!(has_issue(&manifest_issues(dup), "duplicate id"));
    assert!(has_issue(
        &manifest_issues(|t| t[..t.find("[[journey]]").unwrap()].to_string()),
        "no [[journey]]"
    ));
}

#[test]
fn a_native_ui_journey_needs_a_built_binary_and_no_dev_server() {
    let native = |t: String| t.replace("surface = \"engine\"", "surface = \"native-ui\"");
    assert!(manifest_issues(native).is_empty());
    let no_artifacts = |t: String| {
        native(t).replace(
            "artifacts = [{ role = \"engine\", cargo_package = \"demo-engine\" }]\n",
            "",
        )
    };
    assert!(has_issue(&manifest_issues(no_artifacts), "no artifacts"));
    let vite = |t: String| native(t).replace("\"rightkit\", \"cargo\"", "\"pnpm\", \"vite\"");
    assert!(has_issue(&manifest_issues(vite), "dev server"));
    let no_binary = |t: String| native(t).replace(", cargo_package = \"demo-engine\"", "");
    assert!(has_issue(&manifest_issues(no_binary), "no built binary"));
}

#[test]
fn unknown_keys_are_a_parse_error() {
    let t = manifest_text(&["J-001"]).replace("release_critical", "release_critcal");
    assert!(parse_manifest(t.as_bytes()).is_err());
}

#[test]
fn check_reads_files_from_the_app_root() {
    let dir = temp_dir("check");
    fs::create_dir_all(dir.join("tests")).unwrap();
    fs::write(dir.join("tests/j.scenario.toml"), "").unwrap();
    fs::write(dir.join("journeys.toml"), manifest_text(&["J-001"])).unwrap();
    let c = check(&dir).unwrap();
    assert!(c.ok(), "{:?}", c.issues);
    assert!(c.mapping.is_none());
    assert_eq!(c.manifest_sha256.len(), 64);
    fs::write(
        dir.join("journeys-map.toml"),
        "version = 1\n[[behavior]]\nid = \"B-1\"\ntitle = \"t\"\npaths = [\"src/**\"]\njourneys = [\"J-404\"]\n",
    )
    .unwrap();
    let c = check(&dir).unwrap();
    assert!(has_issue(&c.issues, "J-404"), "{:?}", c.issues);
    assert!(c.mapping_sha256.is_some());
    fs::remove_file(dir.join("journeys.toml")).unwrap();
    assert!(
        check(&dir).is_err(),
        "no manifest is an error, never an empty pass"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn mapping_refusals() {
    let (m, _) = parse_manifest(manifest_text(&["J-001"]).as_bytes()).unwrap();
    let map = |body: &str| {
        parse_mapping(format!("version = 1\n{body}").as_bytes())
            .unwrap()
            .0
    };
    assert!(check_mapping(
        &map("[[behavior]]\nid = \"B-1\"\ntitle = \"t\"\npaths = [\"src/**\"]\njourneys = [\"J-001\"]\n"),
        &m
    )
    .is_empty());
    let bad = map("[[behavior]]\nid = \"B-1\"\ntitle = \"t\"\npaths = [\"/abs/**\"]\njourneys = []\n[[exempt]]\npaths = [\"docs/**\"]\nreason = \"\"\n");
    let issues = check_mapping(&bad, &m);
    assert!(has_issue(&issues, "relative glob"));
    assert!(has_issue(&issues, "uncovered"));
    assert!(has_issue(&issues, "reason is empty"));
}
