//! Tests for the journeys module: manifest refusals, the adapter round trip, and one test per
//! way a receipt must never be accepted.
use super::adapter::{
    journey_from_outcomes, journey_from_units, JourneyResult, Record, Reporter, SelectedJourney,
    Selection, StartInfo, UnitOutcome,
};
use super::build_record::{
    Attestation, BuildRecord, FeatureSet, FileHash, MarkerPolicy, RecordedArtifact, Toolchain,
    BUILD_RECORD_FILE, BUILD_RECORD_SCHEMA, REQUEST_ID_SOURCE,
};
use super::manifest::{check, check_manifest, check_mapping, parse_manifest, parse_mapping};
use super::receipt::*;
use super::{
    code, is_rfc3339, ArtifactDigest, Evidence, EvidenceKind, JourneyStatus, Units, RECEIPT_SCHEMA,
};
use crate::evidence::CheckStatus;
use crate::harness::Outcome;
use crate::util::{new_id, sha256_hex};
use crate::worker::{EnvMarker, Survivor, SurvivorOwner, Token};
use std::collections::BTreeMap;
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
        "version = 1\napp = \"demo\"\ncoverage_owners = [\"owner\"]\n\n[[adapter]]\nname = \"demo-adapter\"\nbuild = [\"rustright\", \"cargo\", \"test\", \"--no-run\", \"--message-format=json\"]\nexecutable = { role = \"adapter\", cargo_target = \"journeys_adapter\" }\nartifacts = [{ role = \"engine\", cargo_package = \"demo-engine\" }]\n",
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
                steps: None,
                engine_starts: None,
                survivors: None,
                tokens_held: None,
                world: None,
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
                steps: None,
                engine_starts: None,
                survivors: None,
                tokens_held: None,
                world: None,
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
            command: vec!["rustright".into(), "cargo".into(), "test".into()],
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
        runner: None,
        runner_version: None,
        build_record: None,
        timing: None,
        budget: None,
        explore: None,
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
    let vite = |t: String| native(t).replace("\"rustright\", \"cargo\"", "\"pnpm\", \"vite\"");
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

/// Fixtures for the manifest fields of ADR-0001 Wave 2 (`[build]`, `[budget]`, `[tests]`,
/// `[[bridge]]`, the scenario token lint and the marker flag). Each test writes its own app root.
mod manifest_fields_tests {
    use crate::journeys::manifest::{check, check_lint, check_manifest, parse_manifest};
    use crate::util::new_id;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A throwaway app root holding the given files (root-relative paths), removed on drop.
    struct App(PathBuf);

    impl App {
        fn new(files: &[(&str, &str)]) -> App {
            let dir = std::env::temp_dir().join(format!(
                "rkqa-manifest-fields-{}-{}",
                std::process::id(),
                new_id()
            ));
            for (rel, text) in files {
                let p = dir.join(rel);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(p, text).unwrap();
            }
            App(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for App {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const SCENARIO_JOURNEY: &str = "\n[[journey]]\nid = \"J-1\"\ntitle = \"t\"\nplatforms = [\"windows\"]\nsurface = \"engine\"\nrunner = \"scenario\"\nentry = \"tests/j.scenario.toml\"\ntier = \"fast\"\ndata = \"generated\"\n";
    const BUILD: &str = "\n[build]\ncommand = [\"rustright\", \"cargo\", \"build\", \"-F\", \"qa-test-mode\", \"--message-format=json\"]\nfeatures = [\"qa-test-mode\"]\nartifacts = [{ role = \"app\", cargo_package = \"demo-app\" }, { role = \"engine\", cargo_target = \"demo-engine\", qa_marker = false }]\n";
    const SCENARIO_FILE: &str =
        "name = \"j\"\ntokens = [\"focus\"]\n\n[[step]]\nkind = \"focus_window\"\n";
    const MISSING_ROOT: &str = "/nonexistent-app-root";

    /// A manifest with one scenario journey and the app's `[build]`, followed by `extra`.
    fn manifest(extra: &str) -> String {
        format!("version = 1\napp = \"demo\"\n{SCENARIO_JOURNEY}{BUILD}{extra}")
    }

    #[test]
    fn the_new_sections_parse_and_a_clean_scenario_app_has_no_issues() {
        let text = manifest(
            "\n[budget]\nwarn_until = \"2026-11-06\"\n\n[budget.fast]\nstep_ms = 2000\njourney_ms = 60000\n\n[tests]\nuntagged_baseline = 3\n\n[[bridge]]\napp = \"demo\"\nbinary = \"demo_tests\"\nsources = [\"tests/bridge.rs\"]\ntests = [\"launches_it\"]\nignored_required = [\"slow_one\"]\nremoval_date = \"2026-12-31\"\n",
        );
        let app = App::new(&[
            ("journeys.toml", text.as_str()),
            ("tests/j.scenario.toml", SCENARIO_FILE),
            ("tests/bridge.rs", "fn helper() {}\n"),
        ]);
        let c = check(app.path()).unwrap();
        assert!(c.issues.is_empty(), "{:?}", c.issues);
        assert_eq!(
            c.manifest.tests.as_ref().map(|t| t.untagged_baseline),
            Some(3)
        );
        assert_eq!(c.manifest.bridge[0].removal_date, "2026-12-31");
        assert_eq!(
            c.manifest
                .budget
                .as_ref()
                .unwrap()
                .fast
                .as_ref()
                .unwrap()
                .step_ms,
            Some(2000)
        );
        assert_eq!(
            c.manifest.build.as_ref().unwrap().features,
            vec!["qa-test-mode"]
        );
    }

    #[test]
    fn unknown_keys_in_the_new_sections_are_refused() {
        let bad = [
            "version = 1\napp = \"demo\"\n[build]\ncommand = []\nfeature = [\"x\"]\n",
            "version = 1\napp = \"demo\"\n[build]\ncommand = []\n[[build.artifacts]]\nrole = \"app\"\ncargo_package = \"x\"\nqa_markr = false\n",
            "version = 1\napp = \"demo\"\n[tests]\nuntagged_baseline = 1\nuntagged_basline = 2\n",
            "version = 1\napp = \"demo\"\n[budget]\nwarn_until = \"2026-11-06\"\n[budget.fast]\nstep_msec = 1\n",
            "version = 1\napp = \"demo\"\n[[bridge]]\napp = \"demo\"\nbinary = \"b\"\nremoval_date = \"2026-12-31\"\nremoval = \"2026-12-31\"\n",
        ];
        for text in bad {
            let e = parse_manifest(text.as_bytes()).expect_err("refused");
            assert!(e.0.contains("unknown field"), "{text}: {}", e.0);
        }
    }

    #[test]
    fn a_tests_table_must_freeze_a_count() {
        let e = parse_manifest(b"version = 1\napp = \"demo\"\n[tests]\n").expect_err("refused");
        assert!(e.0.contains("untagged_baseline"), "{}", e.0);
    }

    #[test]
    fn bad_dates_are_refused_and_real_ones_are_not() {
        let refused = [
            (
                "\n[[bridge]]\napp = \"demo\"\nbinary = \"b\"\nremoval_date = \"2026-02-30\"\n",
                "removal_date",
            ),
            (
                "\n[[bridge]]\napp = \"demo\"\nbinary = \"b\"\nremoval_date = \"26-12-31\"\n",
                "removal_date",
            ),
            ("\n[budget]\nwarn_until = \"2026-13-01\"\n", "warn_until"),
            ("\n[budget]\nwarn_until = \"2026-02-29\"\n", "warn_until"),
        ];
        for (extra, needle) in refused {
            let (m, _) = parse_manifest(manifest(extra).as_bytes()).unwrap();
            let issues = check_manifest(&m, Path::new(MISSING_ROOT));
            assert!(
                issues.iter().any(|i| i.contains(needle)),
                "{extra}: {issues:?}"
            );
        }
        let real = manifest(
            "\n[budget]\nwarn_until = \"2028-02-29\"\n\n[[bridge]]\napp = \"demo\"\nbinary = \"b\"\nremoval_date = \"2026-12-31\"\n",
        );
        let (m, _) = parse_manifest(real.as_bytes()).unwrap();
        let issues = check_manifest(&m, Path::new(MISSING_ROOT));
        assert!(
            !issues
                .iter()
                .any(|i| i.contains("removal_date") || i.contains("warn_until")),
            "{issues:?}"
        );
    }

    #[test]
    fn a_scenario_journey_without_a_build_is_an_issue() {
        let bare = format!("version = 1\napp = \"demo\"\n{SCENARIO_JOURNEY}");
        let (m, _) = parse_manifest(bare.as_bytes()).unwrap();
        let issues = check_manifest(&m, Path::new(MISSING_ROOT));
        assert!(
            issues
                .iter()
                .any(|i| i.contains("J-1") && i.contains("[build]")),
            "{issues:?}"
        );
        let (m, _) = parse_manifest(manifest("").as_bytes()).unwrap();
        let issues = check_manifest(&m, Path::new(MISSING_ROOT));
        assert!(!issues.iter().any(|i| i.contains("[build]")), "{issues:?}");
    }

    #[test]
    fn the_build_names_one_feature_set_and_binds_only_built_binaries() {
        let other = manifest("").replace("features = [\"qa-test-mode\"]", "features = [\"other\"]");
        let (m, _) = parse_manifest(other.as_bytes()).unwrap();
        let issues = check_manifest(&m, Path::new(MISSING_ROOT));
        assert!(
            issues.iter().any(|i| i.contains("[build] features")),
            "{issues:?}"
        );

        let by_path = manifest("").replace("cargo_package = \"demo-app\"", "path = \"bin/app\"");
        let (m, _) = parse_manifest(by_path.as_bytes()).unwrap();
        let issues = check_manifest(&m, Path::new(MISSING_ROOT));
        assert!(
            issues.iter().any(|i| i.contains("only a path")),
            "{issues:?}"
        );

        let (m, _) = parse_manifest(manifest("").as_bytes()).unwrap();
        let issues = check_manifest(&m, Path::new(MISSING_ROOT));
        assert!(!issues.iter().any(|i| i.contains("[build]")), "{issues:?}");
    }

    #[test]
    fn qa_marker_defaults_to_true_and_is_read_when_declared() {
        let (m, _) = parse_manifest(manifest("").as_bytes()).unwrap();
        let arts = &m.build.as_ref().unwrap().artifacts;
        assert!(arts[0].qa_marker, "an absent flag requires the marker");
        assert!(!arts[1].qa_marker, "qa_marker = false is read");
    }

    #[test]
    fn an_undeclared_scenario_token_fails_through_check() {
        // `[tests]` is required for an app with scenario journeys (RK Dell), so the fixture declares
        // it: the undeclared token must be the only finding, and a guardrail finding only.
        let text = manifest(
            "
[tests]
untagged_baseline = 0
",
        );
        let app = App::new(&[
            ("journeys.toml", text.as_str()),
            (
                "tests/j.scenario.toml",
                "name = \"j\"\n\n[[step]]\nkind = \"focus_window\"\n",
            ),
        ]);
        let c = check(app.path()).unwrap();
        assert!(
            c.guardrail_issues
                .iter()
                .any(|i| i.contains("undeclared_token")),
            "{:?}",
            c.guardrail_issues
        );
        assert!(!c.ok());
        assert!(
            c.ok_for_run(),
            "a guardrail finding warns a run, it does not stop it"
        );
    }

    #[test]
    fn the_frozen_untagged_count_refuses_a_rise_through_check() {
        let src = "#[test]\nfn untagged() {}\n";
        let cases = [
            ("\n[tests]\nuntagged_baseline = 0\n", true),
            ("\n[tests]\nuntagged_baseline = 1\n", false),
            ("", false),
        ];
        for (extra, rose) in cases {
            let text = manifest(extra);
            let app = App::new(&[
                ("journeys.toml", text.as_str()),
                ("tests/j.scenario.toml", SCENARIO_FILE),
                ("src/lib.rs", src),
            ]);
            let c = check(app.path()).unwrap();
            assert_eq!(c.lint.untagged, 1, "{extra}");
            assert_eq!(
                c.guardrail_issues
                    .iter()
                    .any(|i| i.contains("untagged_rose")),
                rose,
                "{extra}: {:?}",
                c.guardrail_issues
            );
        }
    }

    #[test]
    fn a_bridge_entry_exempts_a_launching_test_only_until_its_removal_date() {
        let src = "// rk-test: contract\n#[test]\nfn launches_it() {\n    let _ = std::process::Command::new(\"demo\").status();\n}\n";
        let text = manifest(
            "\n[[bridge]]\napp = \"demo\"\nbinary = \"demo_tests\"\nsources = [\"tests/bridge.rs\"]\ntests = [\"launches_it\"]\nremoval_date = \"2026-12-31\"\n",
        );
        let app = App::new(&[
            ("journeys.toml", text.as_str()),
            ("tests/j.scenario.toml", SCENARIO_FILE),
            ("tests/bridge.rs", src),
        ]);
        let (m, _) = parse_manifest(text.as_bytes()).unwrap();
        let (live, _) = check_lint(&m, app.path(), "2026-12-31").unwrap();
        assert!(
            !live.iter().any(|i| i.contains("launches_binary")),
            "{live:?}"
        );
        let (expired, _) = check_lint(&m, app.path(), "2027-01-01").unwrap();
        assert!(
            expired.iter().any(|i| i.contains("launches_binary")),
            "{expired:?}"
        );
    }
}

// ---- truthful receipt (W2-F2): one fixture per rule ---------------------------------------------

const ROOT: &str = "/tmp/rkqa/run-1";

fn sample(kind: &str, step: &str, ms: u64) -> StepSample {
    StepSample {
        kind: kind.into(),
        step: step.into(),
        ms,
    }
}

/// The world of the n-th scenario, as the runner records it from the launch it made.
fn world_for(n: u32) -> JourneyWorld {
    let dir = format!("{ROOT}/w{n}");
    let data = format!("{dir}/data");
    let home = format!("{dir}/home");
    let temp = format!("{dir}/tmp");
    let token = format!("{dir}/control.token");
    let marker = EnvMarker {
        name: "RIGHTKIT_QA_WORLD".into(),
        value: format!("w-{n:04}"),
    };
    let env = BTreeMap::from([
        (marker.name.clone(), marker.value.clone()),
        ("RIGHTKIT_DATA_DIR".to_string(), data.clone()),
        ("RIGHTKIT_SUITE_ROOT".to_string(), format!("{home}/suite")),
        ("HOME".to_string(), home.clone()),
        ("TEMP".to_string(), temp.clone()),
        ("RIGHTKIT_QA_CONTROL_TOKEN_FILE".to_string(), token.clone()),
    ]);
    JourneyWorld {
        run_root: ROOT.into(),
        control_socket: format!("{dir}/control.sock"),
        control_token_file: token.clone(),
        port_start: 41000 + 16 * n as u16,
        port_end: 41015 + 16 * n as u16,
        marker,
        data_dir_env: Some("RIGHTKIT_DATA_DIR".into()),
        data_dir_arg: None,
        args: vec![],
        env,
        control_auth: Some(ControlAuth {
            token_file: token,
            owner_only: true,
            token_less_refused: true,
        }),
        dir,
        data_dir: data,
        home,
        temp,
    }
}

/// A build record that agrees with the base fixture's receipt (same roles, digests, sizes, source).
fn record_for(fx: &Fx) -> BuildRecord {
    let art = |role: &str, size: u64, sha: String| RecordedArtifact {
        role: role.into(),
        target: role.into(),
        package: format!("demo-{role}"),
        kind: vec!["bin".into()],
        test_profile: false,
        features: vec![],
        source_path: format!("/target/{role}"),
        store_path: format!("build/bin/{role}/{role}.exe"),
        size,
        sha256: sha,
        qa_marker: false,
        marker: MarkerPolicy::for_role(role),
    };
    BuildRecord {
        schema: BUILD_RECORD_SCHEMA.into(),
        run_id: "run-1".into(),
        app: "demo".into(),
        host_os: "windows".into(),
        host_arch: "x86_64".into(),
        manifest: FileHash {
            path: "journeys.toml".into(),
            sha256: fx.manifest_sha.clone(),
        },
        command: fx.receipt.build.command.clone(),
        broker_path: None,
        started_at: fx.receipt.build.started_at.clone(),
        ended_at: fx.receipt.build.ended_at.clone(),
        source: fx.receipt.source.clone(),
        source_after: fx.receipt.source.clone(),
        broker: Attestation {
            request_id: "096d6843-aaaa-4bbb-8ccc-0123456789ab".into(),
            request_id_source: REQUEST_ID_SOURCE.into(),
            exit_code: 0,
            build_finished: true,
            attested: true,
        },
        lockfiles: vec![FileHash {
            path: "Cargo.lock".into(),
            sha256: rep('1', 64),
        }],
        features: FeatureSet::default(),
        toolchain: Toolchain::default(),
        artifacts: vec![
            art("adapter", 10, rep('b', 64)),
            art("engine", 20, rep('e', 64)),
        ],
    }
}

fn retime(fx: &mut Fx) {
    let t = ReceiptTiming::from_samples(
        fx.receipt
            .journeys
            .iter()
            .flat_map(|j| j.steps.iter().flatten()),
    );
    fx.receipt.timing = Some(t);
}

/// Recompute the claimed counts and verdict, the way the runner does after it fills a receipt.
/// Recompute counts and verdict from the receipt's evidence, as `run::settle_receipt` does: start
/// from a claimed pass so a stale `failed` claim cannot keep itself alive (a claimed failure with
/// no failing rule is itself a `verdict_mismatch`, which would re-fail the receipt on every pass).
fn settle(fx: &mut Fx) {
    fx.receipt.verdict = Verdict::Passed;
    let v = run(fx);
    fx.receipt.counts = v.counts;
    let v = run(fx);
    fx.receipt.verdict = v.verdict;
}

/// A receipt that names the runner and carries every truthful section, consistent with the
/// manifest, the results and a real `build-record.json` in the evidence root.
fn truthful() -> Fx {
    let mut fx = ok();
    // scenario journeys: the runner sees every step and engine start (adapter journeys are covered
    // by `an_adapter_journey_needs_no_steps_or_engine_starts`)
    for j in &mut fx.manifest.journey {
        j.runner = "scenario".into();
    }
    let rec = record_for(&fx);
    let path = fx.dir.join(BUILD_RECORD_FILE);
    rec.write(&path).unwrap();
    let sha = sha256_hex(&fs::read(&path).unwrap());
    fx.receipt.runner = Some(RUNNER.into());
    fx.receipt.runner_version = Some("0.9.0".into());
    fx.receipt.build_record = Some(BuildRecordRef::from_record(&rec, BUILD_RECORD_FILE, &sha));
    for (i, j) in fx.receipt.journeys.iter_mut().enumerate() {
        j.steps = Some(vec![
            sample("cli", "status", 40),
            sample("cli", "login", 60),
            sample("http", "GET /health", 5),
        ]);
        j.engine_starts = Some(EngineStarts::from_total(1));
        j.survivors = Some(vec![]);
        j.tokens_held = Some(vec![]);
        j.world = Some(world_for(i as u32 + 1));
    }
    fx.receipt.isolation.data_dir = format!("{ROOT}/w1/data");
    retime(&mut fx);
    settle(&mut fx);
    fx
}

fn world(fx: &mut Fx, i: usize) -> &mut JourneyWorld {
    fx.receipt.journeys[i].world.as_mut().unwrap()
}

fn survivor() -> Survivor {
    Survivor {
        pid: 4242,
        name: "postright-engine".into(),
        owner: SurvivorOwner::WorldMarker,
        detail: String::new(),
    }
}

fn problem<'a>(v: &'a Validation, c: &str) -> &'a super::Problem {
    v.problems
        .iter()
        .find(|p| p.code == c)
        .unwrap_or_else(|| panic!("no {c} in {:?}", v.problems))
}

#[test]
fn a_complete_truthful_receipt_validates() {
    let fx = truthful();
    let v = run(&fx);
    assert!(v.passed(), "{:?}", v.problems);
    // what it cannot bind is said, never silently passed
    assert!(
        v.unchecked.iter().any(|u| u.starts_with("budget")),
        "{:?}",
        v.unchecked
    );
}

#[test]
fn old_receipts_without_the_new_fields_still_parse_and_validate() {
    let fx = ok();
    let json = serde_json::to_value(&fx.receipt).unwrap();
    for key in [
        "runner",
        "runner_version",
        "build_record",
        "timing",
        "budget",
        "explore",
    ] {
        assert!(
            json.get(key).is_none(),
            "{key} must be absent on an old receipt"
        );
    }
    for j in json["journeys"].as_array().unwrap() {
        for key in [
            "steps",
            "engine_starts",
            "survivors",
            "tokens_held",
            "world",
        ] {
            assert!(j.get(key).is_none(), "{key}");
        }
    }
    let back = parse_receipt(&serde_json::to_vec(&json).unwrap()).unwrap();
    assert_eq!(back, fx.receipt);
    assert!(run(&fx).passed());
}

#[test]
fn a_truthful_receipt_round_trips_through_json() {
    let fx = truthful();
    let back = parse_receipt(&serde_json::to_vec_pretty(&fx.receipt).unwrap()).unwrap();
    assert_eq!(back, fx.receipt);
    let json = serde_json::to_value(&fx.receipt).unwrap();
    assert_eq!(json["runner"], "rightkit-qa");
    assert_eq!(json["timing"]["per_kind"]["cli"]["p95"], 60);
    assert_eq!(json["journeys"][0]["tokens_held"], serde_json::json!([]));
    assert_eq!(json["journeys"][0]["survivors"], serde_json::json!([]));
    assert_eq!(json["journeys"][0]["engine_starts"]["total"], 1);
    assert_eq!(
        json["journeys"][0]["world"]["marker"]["name"],
        "RIGHTKIT_QA_WORLD"
    );
}

#[test]
fn a_receipt_that_names_the_runner_must_carry_every_section() {
    #[allow(clippy::type_complexity)]
    let strips: [(&str, fn(&mut ReceiptJourney)); 5] = [
        ("steps", |j| j.steps = None),
        ("engine_starts", |j| j.engine_starts = None),
        ("survivors", |j| j.survivors = None),
        ("tokens_held", |j| j.tokens_held = None),
        ("world", |j| j.world = None),
    ];
    for (what, strip) in strips {
        let mut fx = truthful();
        strip(&mut fx.receipt.journeys[0]);
        let v = run(&fx);
        refused(&v, truth_code::TRUTH_MISSING);
        let p = problem(&v, truth_code::TRUTH_MISSING);
        assert_eq!(p.id.as_deref(), Some("J-001"), "{what}");
        assert!(p.detail.contains(what), "{what}: {}", p.detail);
        assert_eq!(v.verdict, Verdict::Incomplete, "{what}");
    }
    let mut fx = truthful();
    fx.receipt.build_record = None;
    let v = run(&fx);
    refused(&v, truth_code::TRUTH_MISSING);
    assert_eq!(v.verdict, Verdict::Incomplete);
    let mut fx = truthful();
    fx.receipt.timing = None;
    refused(&run(&fx), truth_code::TRUTH_MISSING);
    // a version nobody can read is held to the rules, not excused from them
    let mut fx = ok();
    fx.receipt.runner = Some(RUNNER.into());
    fx.receipt.runner_version = Some("garbage".into());
    refused(&run(&fx), truth_code::TRUTH_MISSING);
    // a receipt of another runner, or of none, is validated as before
    let mut fx = ok();
    fx.receipt.runner = Some("other-runner".into());
    assert!(run(&fx).passed());
    // a journey that never ran has no sections to miss
    let mut fx = truthful();
    fx.receipt.journeys[1].status = JourneyStatus::Missing;
    fx.receipt.journeys[1].steps = None;
    fx.receipt.journeys[1].world = None;
    let v = run(&fx);
    refused(&v, code::MISSING_RESULT);
    assert!(!v
        .problems
        .iter()
        .any(|p| p.code == truth_code::TRUTH_MISSING));
}

#[test]
fn an_adapter_journey_needs_no_steps_or_engine_starts() {
    let mut fx = truthful();
    fx.manifest.journey[0].runner = "adapter".into();
    fx.receipt.journeys[0].steps = None;
    fx.receipt.journeys[0].engine_starts = None;
    retime(&mut fx);
    settle(&mut fx);
    assert!(run(&fx).passed(), "{:?}", run(&fx).problems);
    // survivors, tokens and the world are still required of it
    fx.receipt.journeys[0].world = None;
    refused(&run(&fx), truth_code::TRUTH_MISSING);
}

// -- build binding

#[test]
fn an_unbound_or_stale_build_is_refused() {
    // the manifest changed since the build
    let mut fx = truthful();
    fx.receipt.build_record.as_mut().unwrap().manifest_sha256 = rep('7', 64);
    refused(&run(&fx), code::BUILD_RECORD_STALE);
    // the build command is not the one recorded
    let mut fx = truthful();
    fx.receipt.build.command = vec!["cargo".into(), "build".into()];
    refused(&run(&fx), code::BUILD_RECORD_STALE);
    // the record was built from another tree
    let mut fx = truthful();
    fx.receipt.build_record.as_mut().unwrap().source_tree = rep('9', 40);
    refused(&run(&fx), code::SOURCE_MISMATCH);
    let mut fx = truthful();
    fx.receipt.build_record.as_mut().unwrap().source_after_tree = rep('9', 40);
    refused(&run(&fx), code::SOURCE_MISMATCH);
    let mut fx = truthful();
    fx.receipt.build_record.as_mut().unwrap().source_head = rep('9', 40);
    refused(&run(&fx), code::SOURCE_MISMATCH);
    // the launched binary is not the one the record holds
    let mut fx = truthful();
    fx.receipt.build_record.as_mut().unwrap().artifacts[1].sha256 = rep('f', 64);
    let v = run(&fx);
    refused(&v, code::ARTIFACT_NOT_FROM_BUILD);
    refused(&v, code::ARTIFACT_MISMATCH);
    // built for another host
    let mut fx = truthful();
    fx.receipt.build_record.as_mut().unwrap().host_os = "macos".into();
    refused(&run(&fx), code::HOST_MISMATCH);
    // a record for another app
    let mut fx = truthful();
    fx.receipt.build_record.as_mut().unwrap().app = "other".into();
    refused(&run(&fx), code::SOURCE_MISMATCH);
    // a binary the record never held
    let mut fx = truthful();
    fx.receipt.build_record.as_mut().unwrap().artifacts.pop();
    let v = run(&fx);
    refused(&v, code::ARTIFACT_NOT_FROM_BUILD);
    // a binary of the record the receipt's build lacks
    let mut fx = truthful();
    fx.receipt.build.artifacts.pop();
    refused(&run(&fx), code::ARTIFACT_MISMATCH);
}

#[test]
fn a_build_that_was_not_attested_is_refused() {
    let tweaks: [fn(&mut BuildRecordRef); 5] = [
        |b| b.attested = false,
        |b| b.exit_code = 1,
        |b| b.build_finished = false,
        |b| b.request_id.clear(),
        |b| b.request_id_source = " ".into(),
    ];
    for tweak in tweaks {
        let mut fx = truthful();
        tweak(fx.receipt.build_record.as_mut().unwrap());
        refused(&run(&fx), code::BUILD_NOT_ATTESTED);
    }
}

#[test]
fn a_launched_binary_without_its_qa_marker_is_refused() {
    let mut fx = truthful();
    let a = &mut fx.receipt.build_record.as_mut().unwrap().artifacts[1];
    a.marker_required = true;
    a.qa_marker = false;
    refused(&run(&fx), code::QA_MARKER_MISSING);
    let mut fx = truthful();
    let a = &mut fx.receipt.build_record.as_mut().unwrap().artifacts[1];
    a.marker_required = true;
    a.qa_marker = true;
    assert!(!run(&fx)
        .problems
        .iter()
        .any(|p| p.code == code::QA_MARKER_MISSING));
}

#[test]
fn the_build_record_summary_must_be_what_the_file_says() {
    // the file was replaced after the summary was taken
    let fx = truthful();
    fs::write(fx.dir.join(BUILD_RECORD_FILE), b"{}").unwrap();
    refused(&run(&fx), code::EVIDENCE_MISMATCH);
    // the file is gone
    let fx = truthful();
    fs::remove_file(fx.dir.join(BUILD_RECORD_FILE)).unwrap();
    refused(&run(&fx), code::EVIDENCE_MISMATCH);
    // another valid record under a re-hashed summary: its fields are not the summary's
    let mut fx = truthful();
    let mut other = record_for(&fx);
    other.broker.request_id = "ffffffff-aaaa-4bbb-8ccc-0123456789ab".into();
    let path = fx.dir.join(BUILD_RECORD_FILE);
    other.write(&path).unwrap();
    fx.receipt.build_record.as_mut().unwrap().sha256 = sha256_hex(&fs::read(&path).unwrap());
    refused(&run(&fx), code::BUILD_RECORD_STALE);
    // an unsafe path is never read
    let mut fx = truthful();
    fx.receipt.build_record.as_mut().unwrap().path = "../build-record.json".into();
    refused(&run(&fx), code::EVIDENCE_MISMATCH);
    // an unattested record file is not a record
    let mut fx = truthful();
    let mut bad = record_for(&fx);
    bad.broker.attested = false;
    let path = fx.dir.join(BUILD_RECORD_FILE);
    fs::write(&path, serde_json::to_vec_pretty(&bad).unwrap()).unwrap();
    fx.receipt.build_record.as_mut().unwrap().sha256 = sha256_hex(&fs::read(&path).unwrap());
    refused(&run(&fx), code::BUILD_NOT_ATTESTED);
}

// -- survivors, engine starts, tokens

#[test]
fn a_survivor_makes_the_journey_unpassable() {
    let mut fx = truthful();
    fx.receipt.journeys[0].survivors = Some(vec![survivor()]);
    let v = run(&fx);
    refused(&v, truth_code::SURVIVOR);
    assert_eq!(
        problem(&v, truth_code::SURVIVOR).id.as_deref(),
        Some("J-001")
    );
    assert!(problem(&v, truth_code::SURVIVOR).detail.contains("4242"));
    // an owner nobody can name is a survivor all the same
    let mut fx = truthful();
    let mut s = survivor();
    s.owner = SurvivorOwner::Unknown;
    fx.receipt.journeys[1].survivors = Some(vec![s]);
    refused(&run(&fx), truth_code::SURVIVOR);
    // an older receipt that records survivors is judged on them too
    let mut fx = ok();
    fx.receipt.journeys[0].survivors = Some(vec![survivor()]);
    refused(&run(&fx), truth_code::SURVIVOR);
}

#[test]
fn engine_starts_over_what_is_allowed_are_refused() {
    // one environment set and no declared cold launch: two starts is one too many
    let mut fx = truthful();
    fx.receipt.journeys[0].engine_starts = Some(EngineStarts::from_total(2));
    let v = run(&fx);
    refused(&v, truth_code::ENGINE_STARTS);
    assert_eq!(
        problem(&v, truth_code::ENGINE_STARTS).id.as_deref(),
        Some("J-001")
    );
    // a declared restart step pays for exactly one more
    let mut fx = truthful();
    fx.receipt.journeys[0]
        .steps
        .as_mut()
        .unwrap()
        .push(sample("restart", "restart engine", 300));
    fx.receipt.journeys[0].engine_starts = Some(EngineStarts::from_total(2));
    retime(&mut fx);
    settle(&mut fx);
    assert!(run(&fx).passed(), "{:?}", run(&fx).problems);
    fx.receipt.journeys[0].engine_starts = Some(EngineStarts::from_total(3));
    refused(&run(&fx), truth_code::ENGINE_STARTS);
    // two environment sets (roles) each start their own
    let mut fx = truthful();
    fx.receipt.journeys[0].engine_starts = Some(EngineStarts::new(BTreeMap::from([
        ("owner".to_string(), 1),
        ("guest".to_string(), 1),
    ])));
    assert!(run(&fx).passed(), "{:?}", run(&fx).problems);
    // per-environment starts that do not add up to the total
    let mut fx = truthful();
    fx.receipt.journeys[0].engine_starts = Some(EngineStarts {
        total: 1,
        by_env: BTreeMap::from([("default".to_string(), 2)]),
        counts: None,
    });
    refused(&run(&fx), truth_code::ENGINE_STARTS);
    // starts with no environment set recorded
    let mut fx = truthful();
    fx.receipt.journeys[0].engine_starts = Some(EngineStarts {
        total: 1,
        by_env: BTreeMap::new(),
        counts: None,
    });
    refused(&run(&fx), truth_code::ENGINE_STARTS);
    // a scenario that used no engine is fine
    let mut fx = truthful();
    fx.receipt.journeys[0].engine_starts = Some(EngineStarts::from_total(0));
    assert!(run(&fx).passed());
}

#[test]
fn tokens_held_must_be_the_tokens_the_manifest_declares() {
    // holds a token the journey does not declare
    let mut fx = truthful();
    fx.receipt.journeys[0].tokens_held = Some(vec![Token::Focus]);
    let v = run(&fx);
    refused(&v, truth_code::TOKENS_MISMATCH);
    assert!(problem(&v, truth_code::TOKENS_MISMATCH)
        .detail
        .contains("focus"));
    // declares two, held one
    let mut fx = truthful();
    fx.manifest.journey[0].tokens = vec!["focus".into(), "audio".into()];
    fx.receipt.journeys[0].tokens_held = Some(vec![Token::Focus]);
    let v = run(&fx);
    refused(&v, truth_code::TOKENS_MISMATCH);
    assert!(problem(&v, truth_code::TOKENS_MISMATCH)
        .detail
        .contains("audio"));
    // held both, in another order
    fx.receipt.journeys[0].tokens_held = Some(vec![Token::Audio, Token::Focus]);
    assert!(run(&fx).passed(), "{:?}", run(&fx).problems);
    // held twice
    fx.receipt.journeys[0].tokens_held = Some(vec![Token::Focus, Token::Focus, Token::Audio]);
    refused(&run(&fx), truth_code::TOKENS_MISMATCH);
}

// -- timing and budget

#[test]
fn timing_is_nearest_rank_per_kind_and_lists_the_ten_slowest() {
    let samples: Vec<StepSample> = (1..=100u64)
        .map(|i| sample("cli", &format!("s{i:03}"), i))
        .chain([sample("http", "slow-get", 500)])
        .collect();
    let t = ReceiptTiming::from_samples(&samples);
    assert_eq!(
        t.per_kind["cli"],
        KindStats {
            p50: 50,
            p95: 95,
            max: 100
        }
    );
    let hundred: Vec<u64> = (1..=100).collect();
    assert_eq!(Some(t.per_kind["cli"].p95), crate::budget::p95(&hundred));
    assert_eq!(
        t.per_kind["http"],
        KindStats {
            p50: 500,
            p95: 500,
            max: 500
        }
    );
    assert_eq!(t.slowest.len(), SLOWEST_STEPS);
    assert_eq!(
        t.slowest[0],
        SlowStep {
            step: "slow-get".into(),
            ms: 500
        }
    );
    assert_eq!(t.slowest[1].ms, 100);
    assert_eq!(t.slowest[9].ms, 92);
    // ties: longest first, then by label
    let tie = [sample("cli", "b", 7), sample("cli", "a", 7)];
    assert_eq!(ReceiptTiming::from_samples(&tie).slowest[0].step, "a");
    assert_eq!(
        ReceiptTiming::from_samples(std::iter::empty::<&StepSample>()),
        ReceiptTiming::default()
    );
}

#[test]
fn a_claimed_timing_the_steps_do_not_give_is_refused() {
    let mut fx = truthful();
    fx.receipt
        .timing
        .as_mut()
        .unwrap()
        .per_kind
        .get_mut("cli")
        .unwrap()
        .p95 = 1;
    let v = run(&fx);
    refused(&v, truth_code::TIMING_MISMATCH);
    assert!(problem(&v, truth_code::TIMING_MISMATCH)
        .detail
        .contains("cli"));
    let mut fx = truthful();
    fx.receipt.timing.as_mut().unwrap().slowest.pop();
    refused(&run(&fx), truth_code::TIMING_MISMATCH);
    // the steps changed and the claim did not follow
    let mut fx = truthful();
    fx.receipt.journeys[0].steps.as_mut().unwrap()[0].ms = 9_000;
    refused(&run(&fx), truth_code::TIMING_MISMATCH);
    // a step kind no step has
    let mut fx = truthful();
    fx.receipt.timing.as_mut().unwrap().per_kind.insert(
        "ghost".into(),
        KindStats {
            p50: 1,
            p95: 1,
            max: 1,
        },
    );
    let v = run(&fx);
    refused(&v, truth_code::TIMING_MISMATCH);
    assert!(problem(&v, truth_code::TIMING_MISMATCH)
        .detail
        .contains("ghost"));
}

/// J-001's `login` takes 3 s against a `cli` step limit of 2 s; the window ends on `window_end`.
fn slow_login(window_end: &str) -> Fx {
    let mut fx = truthful();
    fx.receipt.journeys[0].steps.as_mut().unwrap()[1] = sample("cli", "login", 3_000);
    retime(&mut fx);
    let limits = BTreeMap::from([(
        "cli".to_string(),
        BudgetLimits {
            warn_ms: 2_000,
            enforce_ms: 2_000,
        },
    )]);
    fx.receipt.budget = Some(
        ReceiptBudget::build(
            window_end,
            limits,
            None,
            "fast",
            &fx.receipt.journeys,
            &fx.receipt.ended_at,
        )
        .unwrap(),
    );
    settle(&mut fx);
    fx
}

#[test]
fn slow_is_recorded_inside_the_window_and_does_not_fail() {
    for end in ["2026-10-31", "2026-10-21"] {
        // the day the run ended is still inside the window
        let fx = slow_login(end);
        let b = fx.receipt.budget.as_ref().unwrap();
        assert_eq!(b.verdicts.len(), 1, "{end}");
        assert_eq!(b.verdicts[0].outcome, BudgetOutcome::Slow);
        assert_eq!(b.verdicts[0].label, "login");
        assert_eq!(b.verdicts[0].ms, 3_000);
        let v = run(&fx);
        assert!(v.passed(), "{end}: {:?}", v.problems);
        assert!(v.unchecked.iter().any(|u| u.contains("window_end")));
    }
}

#[test]
fn over_budget_after_the_window_fails_and_names_the_step() {
    let fx = slow_login("2026-10-20");
    assert_eq!(
        fx.receipt.budget.as_ref().unwrap().verdicts[0].outcome,
        BudgetOutcome::OverBudget
    );
    let v = run(&fx);
    refused(&v, truth_code::OVER_BUDGET);
    let p = problem(&v, truth_code::OVER_BUDGET);
    assert_eq!(p.id.as_deref(), Some("J-001"));
    assert!(
        p.detail.contains("login") && p.detail.contains("OVER BUDGET"),
        "{}",
        p.detail
    );
    // under the enforcement threshold after the window: ok
    let mut fx = slow_login("2026-10-20");
    fx.receipt
        .budget
        .as_mut()
        .unwrap()
        .step_limits
        .get_mut("cli")
        .unwrap()
        .enforce_ms = 4_000;
    fx.receipt.budget.as_mut().unwrap().verdicts.clear();
    settle(&mut fx);
    assert!(run(&fx).passed(), "{:?}", run(&fx).problems);
}

#[test]
fn a_budget_that_hides_an_offender_is_refused_and_still_fails() {
    // the offending entry is left out
    let mut fx = slow_login("2026-10-20");
    fx.receipt.budget.as_mut().unwrap().verdicts.clear();
    let v = run(&fx);
    refused(&v, truth_code::BUDGET_MISMATCH);
    assert!(v.problems.iter().any(|p| p.code == truth_code::OVER_BUDGET));
    // the entry is downgraded to a warning
    let mut fx = slow_login("2026-10-20");
    fx.receipt.budget.as_mut().unwrap().verdicts[0].outcome = BudgetOutcome::Slow;
    let v = run(&fx);
    refused(&v, truth_code::BUDGET_MISMATCH);
    assert!(v.problems.iter().any(|p| p.code == truth_code::OVER_BUDGET));
    // an entry for a step that was within budget
    let mut fx = slow_login("2026-10-31");
    let mut extra = fx.receipt.budget.as_ref().unwrap().verdicts[0].clone();
    extra.label = "status".into();
    extra.ms = 40;
    fx.receipt.budget.as_mut().unwrap().verdicts.push(extra);
    refused(&run(&fx), truth_code::BUDGET_MISMATCH);
}

#[test]
fn journey_budgets_are_judged_from_the_recorded_durations() {
    let mut fx = truthful();
    fx.receipt.budget = Some(
        ReceiptBudget::build(
            "2026-10-20",
            BTreeMap::new(),
            Some(BudgetLimits {
                warn_ms: 2,
                enforce_ms: 2,
            }),
            "fast",
            &fx.receipt.journeys,
            &fx.receipt.ended_at,
        )
        .unwrap(),
    );
    assert_eq!(fx.receipt.budget.as_ref().unwrap().verdicts.len(), 2);
    let v = run(&fx);
    assert_eq!(
        v.problems
            .iter()
            .filter(|p| p.code == truth_code::OVER_BUDGET)
            .count(),
        2
    );
    refused(&v, truth_code::OVER_BUDGET);
}

#[test]
fn a_malformed_budget_section_is_refused() {
    let mut fx = slow_login("2026-10-31");
    fx.receipt.budget.as_mut().unwrap().window_end = "2026-13-45".into();
    refused(&run(&fx), truth_code::BUDGET_MISMATCH);
    let mut fx = slow_login("2026-10-31");
    fx.receipt
        .budget
        .as_mut()
        .unwrap()
        .step_limits
        .get_mut("cli")
        .unwrap()
        .warn_ms = 0;
    refused(&run(&fx), truth_code::BUDGET_MISMATCH);
    assert!(ReceiptBudget::build(
        "2026-02-30",
        BTreeMap::new(),
        None,
        "fast",
        &[],
        "2026-10-21T10:05:00Z"
    )
    .is_err());
    assert!(ReceiptBudget::build("2026-10-31", BTreeMap::new(), None, "fast", &[], "").is_err());
}

// -- isolation recorded from the launch

#[test]
fn isolation_is_judged_from_the_recorded_launch_not_asserted() {
    // the app was told another data directory than the one recorded
    let mut fx = truthful();
    world(&mut fx, 0).env.insert(
        "RIGHTKIT_DATA_DIR".into(),
        "/home/user/.local/share/postright".into(),
    );
    refused(&run(&fx), code::ISOLATION);
    // the variable the data directory went through is not in the launch environment
    let mut fx = truthful();
    world(&mut fx, 0).env.remove("RIGHTKIT_DATA_DIR");
    refused(&run(&fx), code::ISOLATION);
    // a recorded data directory outside the run root
    let mut fx = truthful();
    let w = world(&mut fx, 0);
    w.data_dir = "/home/user/data".into();
    w.env
        .insert("RIGHTKIT_DATA_DIR".into(), "/home/user/data".into());
    refused(&run(&fx), code::ISOLATION);
    // a launch environment that points HOME or TEMP at the real thing
    for key in ["HOME", "TEMP", "RIGHTKIT_SUITE_ROOT"] {
        let mut fx = truthful();
        world(&mut fx, 0)
            .env
            .insert(key.into(), "/home/user".into());
        refused(&run(&fx), code::ISOLATION);
    }
    // a path that escapes the run root by name or by `..`
    let mut fx = truthful();
    world(&mut fx, 0).temp = format!("{ROOT}/w1/../../elsewhere");
    refused(&run(&fx), code::ISOLATION);
    let mut fx = truthful();
    world(&mut fx, 0).home = "/tmp/rkqa/run-10/w1/home".into();
    refused(&run(&fx), code::ISOLATION);
    // the receipt-level data_dir must be one of the launches' world
    let mut fx = truthful();
    fx.receipt.isolation.data_dir = "/tmp/rightkit-qa/demo-1".into();
    refused(&run(&fx), code::ISOLATION);
    // a port range that is not a range
    let mut fx = truthful();
    world(&mut fx, 0).port_start = 80;
    refused(&run(&fx), code::ISOLATION);
    // a relative run root names nothing
    let mut fx = truthful();
    world(&mut fx, 0).run_root = "rkqa/run-1".into();
    refused(&run(&fx), code::ISOLATION);
}

#[test]
fn the_data_directory_may_go_through_an_argument() {
    let mut fx = truthful();
    let w = world(&mut fx, 0);
    w.data_dir_env = None;
    w.data_dir_arg = Some("--data-dir".into());
    w.args = vec!["--data-dir".into(), w.data_dir.clone()];
    assert!(run(&fx).passed(), "{:?}", run(&fx).problems);
    // `--flag=value` form
    let w = world(&mut fx, 0);
    w.args = vec![format!("--data-dir={}", w.data_dir)];
    assert!(run(&fx).passed(), "{:?}", run(&fx).problems);
    // another directory than the recorded one
    world(&mut fx, 0).args = vec!["--data-dir".into(), "/home/user/real".into()];
    refused(&run(&fx), code::ISOLATION);
    // the flag is not among the arguments
    world(&mut fx, 0).args = vec![];
    refused(&run(&fx), code::ISOLATION);
    // nothing records how the app was told
    let w = world(&mut fx, 0);
    w.data_dir_env = None;
    w.data_dir_arg = None;
    refused(&run(&fx), code::ISOLATION);
}

#[test]
fn two_scenarios_never_share_a_world() {
    // the same marker
    let mut fx = truthful();
    let m = world(&mut fx, 0).marker.clone();
    let b = world(&mut fx, 1);
    b.marker = m.clone();
    b.env.insert(m.name.clone(), m.value.clone());
    refused(&run(&fx), code::ISOLATION);
    // the same data directory
    let mut fx = truthful();
    let d = world(&mut fx, 0).data_dir.clone();
    let b = world(&mut fx, 1);
    b.data_dir = d.clone();
    b.env.insert("RIGHTKIT_DATA_DIR".into(), d);
    let v = run(&fx);
    refused(&v, code::ISOLATION);
    assert!(
        v.problems
            .iter()
            .any(|p| p.code == code::ISOLATION && p.detail.contains("shares")),
        "{:?}",
        v.problems
    );
    // the marker is not in the launch environment at all
    let mut fx = truthful();
    world(&mut fx, 0).env.remove("RIGHTKIT_QA_WORLD");
    refused(&run(&fx), code::ISOLATION);
    // two run roots in one receipt
    let mut fx = truthful();
    world(&mut fx, 1).run_root = "/tmp/rkqa/run-2".into();
    refused(&run(&fx), code::ISOLATION);
}

#[test]
fn the_control_token_must_be_owner_only_enforced_and_the_one_launched() {
    let mut fx = truthful();
    world(&mut fx, 0)
        .control_auth
        .as_mut()
        .unwrap()
        .token_less_refused = false;
    let v = run(&fx);
    refused(&v, truth_code::CONTROL_AUTH);
    assert!(problem(&v, truth_code::CONTROL_AUTH)
        .detail
        .contains("re-pin"));
    let mut fx = truthful();
    world(&mut fx, 0).control_auth.as_mut().unwrap().owner_only = false;
    refused(&run(&fx), truth_code::CONTROL_AUTH);
    let mut fx = truthful();
    world(&mut fx, 0).control_auth.as_mut().unwrap().token_file =
        format!("{ROOT}/w2/control.token");
    refused(&run(&fx), truth_code::CONTROL_AUTH);
    // the launch never named the token file
    let mut fx = truthful();
    world(&mut fx, 0)
        .env
        .remove("RIGHTKIT_QA_CONTROL_TOKEN_FILE");
    refused(&run(&fx), truth_code::CONTROL_AUTH);
    // a native-ui journey drives the control server and must record the handling
    let mut fx = truthful();
    fx.manifest.journey[0].surface = "native-ui".into();
    assert!(run(&fx).passed(), "{:?}", run(&fx).problems);
    world(&mut fx, 0).control_auth = None;
    refused(&run(&fx), truth_code::CONTROL_AUTH);
}

// -- explore never makes a receipt

#[test]
fn a_receipt_carrying_the_explore_marker_is_refused() {
    let mut fx = truthful();
    fx.receipt.explore = Some(true);
    refused(&run(&fx), truth_code::EXPLORE_RECEIPT);
    // the key is the marker, whatever its value
    let mut fx = truthful();
    fx.receipt.explore = Some(false);
    refused(&run(&fx), truth_code::EXPLORE_RECEIPT);
    // and on a receipt that predates the runner field
    let mut fx = ok();
    fx.receipt.explore = Some(true);
    refused(&run(&fx), truth_code::EXPLORE_RECEIPT);
    // through the wire form
    let mut json = serde_json::to_value(&ok().receipt).unwrap();
    json["explore"] = serde_json::json!(true);
    let parsed = parse_receipt(&serde_json::to_vec(&json).unwrap()).unwrap();
    assert_eq!(parsed.explore, Some(true));
    let mut fx = ok();
    fx.receipt = parsed;
    refused(&run(&fx), truth_code::EXPLORE_RECEIPT);
    // even a receipt of another schema shows the marker
    let mut fx = ok();
    fx.receipt.schema = "rightkit.journeys.receipt/2".into();
    fx.receipt.explore = Some(true);
    let v = run(&fx);
    refused(&v, code::SCHEMA);
    refused(&v, truth_code::EXPLORE_RECEIPT);
}

#[test]
fn write_receipt_refuses_an_explore_run_and_writes_the_rest_whole() {
    let fx = truthful();
    let out = fx.dir.join("receipt.json");
    write_receipt(&out, &fx.receipt).unwrap();
    assert_eq!(parse_receipt(&fs::read(&out).unwrap()).unwrap(), fx.receipt);
    let mut explore = fx.receipt.clone();
    explore.explore = Some(true);
    let refused_path = fx.dir.join("explore-receipt.json");
    assert!(write_receipt(&refused_path, &explore).is_err());
    assert!(!refused_path.exists(), "no receipt file for an explore run");
}

#[test]
fn an_explore_run_records_a_loadable_scenario_and_no_receipt() {
    let dir = temp_dir("explore");
    let steps = vec![
        ExploreStep {
            kind: "cli".into(),
            label: Some("status".into()),
            params: BTreeMap::from([("args".to_string(), serde_json::json!(["status", "--json"]))]),
        },
        ExploreStep {
            kind: "http".into(),
            label: None,
            params: BTreeMap::from([
                (
                    "url".to_string(),
                    serde_json::json!("http://127.0.0.1:41000/health"),
                ),
                ("expect_status".to_string(), serde_json::json!(200)),
            ]),
        },
    ];
    let path = write_explore_scenario(&dir, "login-flow", "engine", &["focus".to_string()], &steps)
        .unwrap();
    assert_eq!(path, dir.join("login-flow.scenario.toml"));
    let loaded = crate::scenario::Scenario::load(&path).unwrap();
    assert_eq!(loaded.name, "login-flow");
    assert_eq!(loaded.target, "engine");
    assert_eq!(loaded.tokens, vec!["focus".to_string()]);
    assert_eq!(loaded.steps.len(), 2);
    assert_eq!(loaded.steps[0].kind, "cli");
    assert_eq!(loaded.steps[0].label.as_deref(), Some("status"));
    assert_eq!(
        loaded.steps[0].params.get("args"),
        Some(&serde_json::json!(["status", "--json"]))
    );
    assert_eq!(loaded.steps[1].kind, "http");
    assert_eq!(
        loaded.steps[1].params.get("expect_status"),
        Some(&serde_json::json!(200))
    );
    assert!(fs::read_to_string(&path).unwrap().contains("Not evidence"));
    // the directory holds the scenario and nothing else: no receipt, no results
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["login-flow.scenario.toml".to_string()]);
    // an existing scenario is never overwritten
    assert!(write_explore_scenario(&dir, "login-flow", "engine", &[], &steps).is_err());
    // what cannot be recorded is refused, and leaves no file behind
    let one = |params: BTreeMap<String, serde_json::Value>| {
        vec![ExploreStep {
            kind: "cli".into(),
            label: None,
            params,
        }]
    };
    let null = one(BTreeMap::from([("x".to_string(), serde_json::Value::Null)]));
    assert!(write_explore_scenario(&dir, "has-null", "engine", &[], &null).is_err());
    let clash = one(BTreeMap::from([(
        "kind".to_string(),
        serde_json::json!("x"),
    )]));
    assert!(write_explore_scenario(&dir, "clash", "engine", &[], &clash).is_err());
    assert!(write_explore_scenario(&dir, "no-steps", "engine", &[], &[]).is_err());
    assert!(write_explore_scenario(&dir, "Bad Name", "engine", &[], &steps).is_err());
    assert!(write_explore_scenario(&dir, "bad-target", "mainframe", &[], &steps).is_err());
    assert!(write_explore_scenario(
        &dir,
        "bad-token",
        "engine",
        &["not-a-token".to_string()],
        &steps
    )
    .is_err());
    let after: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(after, vec!["login-flow.scenario.toml".to_string()]);
    let _ = fs::remove_dir_all(&dir);
}

// -- errored, not run and skipped never count

#[test]
fn only_a_passed_check_is_a_passed_journey() {
    for (check, journey) in [
        (CheckStatus::Passed, JourneyStatus::Passed),
        (CheckStatus::Failed, JourneyStatus::Failed),
        (CheckStatus::Skipped, JourneyStatus::Skipped),
        (CheckStatus::Errored, JourneyStatus::Errored),
        (CheckStatus::NotRun, JourneyStatus::Missing),
    ] {
        assert_eq!(journey_status_of(check), journey);
        assert_eq!(
            journey_status_of(check) == JourneyStatus::Passed,
            check.is_pass()
        );
    }
}

#[test]
fn errored_skipped_and_not_run_journeys_never_count_in_a_truthful_receipt() {
    let mut fx = truthful();
    fx.receipt.journeys[0].status = journey_status_of(CheckStatus::Errored);
    fx.receipt.journeys[0].reason = Some("the worker died".into());
    let v = run(&fx);
    refused(&v, code::JOURNEY_FAILED);
    assert_eq!(v.counts.errored, 1);
    let mut fx = truthful();
    fx.receipt.journeys[0].status = journey_status_of(CheckStatus::Skipped);
    fx.receipt.journeys[0].reason = Some("not today".into());
    refused(&run(&fx), code::SKIPPED);
    let mut fx = truthful();
    fx.receipt.journeys[0].status = journey_status_of(CheckStatus::NotRun);
    fx.receipt.journeys[0].reason = Some("never started".into());
    let v = run(&fx);
    refused(&v, code::MISSING_RESULT);
    assert_eq!(v.counts.missing, 1);
    // a required journey id with no result at all
    let mut fx = truthful();
    fx.receipt.journeys.retain(|j| j.id != "J-002");
    refused(&run(&fx), code::MISSING_RESULT);
}

// -- budget window bound to the manifest (W2-R)

fn budget_decl(warn_until: &str) -> crate::budget::BudgetDecl {
    crate::budget::BudgetDecl {
        warn_until: warn_until.into(),
        fast: None,
        full: None,
        paid: None,
    }
}

#[test]
fn the_budget_window_is_bound_to_the_manifests_warn_until() {
    // the receipt's window is the manifest's: accepted, and no longer listed as an unchecked claim
    let mut fx = slow_login("2026-10-31");
    fx.manifest.budget = Some(budget_decl("2026-10-31"));
    let v = run(&fx);
    assert!(v.passed(), "{:?}", v.problems);
    assert!(
        !v.unchecked.iter().any(|u| u.contains("window_end")),
        "{:?}",
        v.unchecked
    );
    // a runner that stretched the window to turn an over-budget step into `slow` is refused
    let mut fx = slow_login("2026-12-31");
    fx.manifest.budget = Some(budget_decl("2026-10-20"));
    let v = run(&fx);
    refused(&v, truth_code::BUDGET_MISMATCH);
    assert!(problem(&v, truth_code::BUDGET_MISMATCH)
        .detail
        .contains("warn_until"));
    // without a manifest [budget] the window stays the receipt's own claim and says so
    let fx = slow_login("2026-10-31");
    assert!(run(&fx).unchecked.iter().any(|u| u.contains("window_end")));
}

#[test]
fn the_control_token_variable_is_the_one_the_world_exports() {
    assert_eq!(
        crate::control::token::ENV_AUTH_FILE,
        crate::pool::ENV_CONTROL_TOKEN_FILE
    );
}
