use rightkit_memory::membrane::*;
use std::path::Path;
use std::time::{Duration, Instant};

fn fake() -> MembraneCli {
    MembraneCli::new(env!("CARGO_BIN_EXE_fake-membrane").into())
}

/// Test dirs live under D:/scratch when present (disk-hygiene rule).
fn scratch() -> tempfile::TempDir {
    let base = Path::new("D:/scratch/membrane-cli");
    if std::fs::create_dir_all(base).is_ok() {
        tempfile::Builder::new().tempdir_in(base).unwrap()
    } else {
        tempfile::tempdir().unwrap()
    }
}

#[test]
fn missing_binary_locates_none_and_falls_back_to_unavailable() {
    let dir = scratch();
    let missing = dir.path().join("nope.exe");
    assert!(MembraneCli::locate_with(Some(missing.clone().into()), None, None).is_none());
    assert!(
        MembraneCli::locate_with(None, Some(dir.path().into()), Some(dir.path().into())).is_none()
    );
    let err = MembraneCli::new(missing).status().unwrap_err();
    assert!(matches!(err, MembraneError::Spawn(_)));
    let u = Unavailable;
    let c = u.context_pull(dir.path(), "x", None, None).unwrap();
    assert!(c.text.is_empty());
    assert_eq!(c.off_reason.as_deref(), Some("off: pulse not installed"));
    assert!(matches!(
        u.memory_push("t"),
        Err(MembraneError::Unavailable(_))
    ));
}

#[test]
fn env_override_locates_existing_binary() {
    let bin = env!("CARGO_BIN_EXE_fake-membrane");
    assert_eq!(
        MembraneCli::locate_with(Some(bin.into()), None, None).unwrap(),
        Path::new(bin)
    );
}

#[test]
fn timeout_kills_the_child() {
    // Mode is scoped to this child via Command::env; the test process env is untouched,
    // so parallel tests cannot inherit "hang".
    let cli = fake()
        .with_env("FAKE_MEMBRANE_MODE", "hang")
        .with_timeout(Duration::from_millis(300));
    let t = Instant::now();
    let err = cli.status().unwrap_err();
    assert!(matches!(err, MembraneError::Timeout(_)), "{err:?}");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "kill must return promptly"
    );
}

#[test]
fn notch_state_precedes_static_paths() {
    let dir = scratch();
    let bin = env!("CARGO_BIN_EXE_fake-membrane");
    let lad = dir.path().join("lad");
    let home = dir.path().join("home");
    let notch = if cfg!(windows) {
        lad.join("Pulse")
    } else {
        home.join("Library/Application Support/Pulse")
    };
    std::fs::create_dir_all(&notch).unwrap();
    let doc = serde_json::json!({"membraneBinary": {"path": bin}});
    std::fs::write(notch.join("notch-state.json"), doc.to_string()).unwrap();
    assert_eq!(
        MembraneCli::locate_with(None, Some(lad.clone().into()), Some(home.clone().into()))
            .unwrap(),
        Path::new(bin)
    );
    // A recorded path that no longer exists is ignored.
    let gone = serde_json::json!({"membraneBinary": {"path": dir.path().join("gone")}});
    std::fs::write(notch.join("notch-state.json"), gone.to_string()).unwrap();
    assert!(MembraneCli::locate_with(None, Some(lad.into()), Some(home.into())).is_none());
}

#[test]
fn where_json_reports_binary() {
    let w = fake().where_binary().unwrap();
    assert!(w.binary.is_file());
    assert_eq!(w.membrane_source_commit.as_deref(), Some("abc123"));
    assert_eq!(w.state_root.as_deref(), Some("/state"));
    // The path that was run is the path `where` reports.
    fake().verify().unwrap();
}

#[test]
fn adapt_commands_send_documents_and_return_result() {
    let dir = scratch();
    let log = dir.path().join("args.log");
    let cli = fake().with_env("FAKE_MEMBRANE_ARGS_LOG", &log);
    let req = serde_json::json!({"operation": "analyze", "observations": []});
    // The document travels on stdin and comes back under `result`.
    assert_eq!(cli.adapt_observe(&req).unwrap()["echo"], req);
    let r = cli
        .adapt_recall("tabs", 3, "global", &["lang=rust".to_owned()])
        .unwrap();
    assert_eq!(r["query"], "tabs");
    let ins = dir.path().join("insights.json");
    let applied = cli.adapt_apply_insights(&ins, "inst-1", true).unwrap();
    assert_eq!(applied["accepted_record_ids"][0], "r1");
    cli.adapt_apply_manifest(&ins, false).unwrap();
    let review = cli
        .adapt_taste_review(&ins, "inst-1", "2026-01-01T00:00:00Z", Some("abc"))
        .unwrap();
    assert_eq!(review["manifest"]["pending"], true);
    let logged = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<Vec<&str>> = logged
        .lines()
        .map(|l| l.split('\u{1f}').collect())
        .collect();
    assert_eq!(&lines[0][..4], ["adapt", "observe", "--stdin", "--json"]);
    assert!(lines[1].windows(2).any(|w| w == ["-k", "3"]));
    assert!(lines[1].windows(2).any(|w| w == ["--scope", "global"]));
    assert!(lines[1]
        .windows(2)
        .any(|w| w == ["--dimension", "lang=rust"]));
    assert!(lines[2].contains(&"--insights") && lines[2].contains(&"--dry-run"));
    assert!(lines[2]
        .windows(2)
        .any(|w| w == ["--installation-id", "inst-1"]));
    assert!(lines[3].contains(&"--manifest") && !lines[3].contains(&"--dry-run"));
    assert_eq!(lines[4][1], "taste-review");
    assert!(lines[4]
        .windows(2)
        .any(|w| w == ["--canonical-pool-sha256", "abc"]));
    assert!(lines[4]
        .windows(2)
        .any(|w| w == ["--created-at", "2026-01-01T00:00:00Z"]));
}

#[test]
fn transcript_append_and_tip_are_typed() {
    let cli = fake();
    let events = serde_json::json!([{"type": "host_event_v1"}, {"type": "host_event_v1"}]);
    match cli
        .transcript_append("s1", 5, Some("global"), &events)
        .unwrap()
    {
        TranscriptAppend::Committed {
            session,
            cursor,
            replayed,
            receipt,
        } => {
            assert_eq!((session.as_str(), cursor, replayed), ("s1", 5, false));
            assert_eq!(receipt["inserted"], 2);
            assert_eq!(receipt["lastSeq"], 7);
        }
        other => panic!("{other:?}"),
    }
    // A stale cursor is a typed conflict carrying the stream's end, not an error string.
    match cli.transcript_append("s1", 2, None, &events).unwrap() {
        TranscriptAppend::Conflict(c) => {
            assert_eq!(c.current_cursor, Some(5));
            assert_eq!(c.status, Some(409));
            assert_eq!(c.code, "transcript_cursor_mismatch");
        }
        other => panic!("{other:?}"),
    }
    match cli.transcript_append("gone", 5, None, &events).unwrap() {
        TranscriptAppend::Conflict(c) => {
            assert_eq!(c.current_cursor, None);
            assert_eq!(c.status, Some(410));
            assert_eq!(c.code, "transcript_session_gone");
        }
        other => panic!("{other:?}"),
    }
    let tip = cli.transcript_tip("s1").unwrap();
    assert_eq!(
        (tip.current_cursor, tip.prefix_digest.as_deref()),
        (5, Some("d1"))
    );
    assert!(matches!(
        cli.transcript_tip("gone").unwrap_err(),
        MembraneError::Usage(_)
    ));
    // A plain exit-2 refusal with no conflict document stays an error.
    let usage = fake().with_env("FAKE_MEMBRANE_MODE", "usage");
    assert!(matches!(
        usage.transcript_append("s1", 5, None, &events).unwrap_err(),
        MembraneError::Usage(_)
    ));
    assert!(matches!(
        Unavailable.transcript_tip("s1").unwrap_err(),
        MembraneError::Unavailable(_)
    ));
    assert!(matches!(
        Unavailable.adapt_recall("q", 1, "global", &[]).unwrap_err(),
        MembraneError::Unavailable(_)
    ));
}

#[test]
fn memory_delete_of_unknown_id_is_not_an_error() {
    let del = fake().memory_delete("missing").unwrap();
    assert!(!del.deleted);
    assert_eq!(del.id, "missing");
}

#[test]
fn success_parses_every_op() {
    let dir = scratch();
    let cli = fake();
    let c = cli
        .context_pull(dir.path(), "task", Some(100), None)
        .unwrap();
    assert_eq!(c.text, "fake context line\nsecond line");
    assert!(c.off_reason.is_none());
    assert_eq!(c.reason.as_deref(), Some("retrieved"));
    assert_eq!(c.detail["hits"], 2);
    assert_eq!(cli.memory_push("hi").unwrap()["stored"], true);
    let list = cli.memory_list(5, None).unwrap();
    assert_eq!(list.items.len(), 2);
    assert_eq!(list.items[0].id, "mem-1");
    assert_eq!(list.items[1].tier, "session");
    assert_eq!(list.completeness["state"], "complete");
    let found = cli
        .memory_search("q", &["durable".into()], 5, None)
        .unwrap();
    assert_eq!(found.items[0].score, Some(0.75));
    assert_eq!(found.items[0].content.as_deref(), Some("first"));
    assert_eq!(found.items[0].scope.as_deref(), Some("global"));
    assert_eq!(found.tiers, ["semantic"]);
    assert_eq!(found.pool, Some(9));
    assert!(found.more);
    let del = cli.memory_delete("mem-1").unwrap();
    assert!(del.deleted && del.id == "mem-1");
    let bp = cli.blueprint_search("q", 3, None).unwrap();
    assert_eq!(bp.schema, "membrane.blueprint.search.v1");
    assert_eq!(bp.body["results"][0]["id"], "n1");
    let st = cli.status().unwrap();
    assert_eq!(st.product_version, "9.9");
    assert_eq!(st.mode, "in_process");
    assert_eq!(st.db_present, Some(true));
}

#[test]
fn repo_tier_and_json_flags_reach_the_binary() {
    let dir = scratch();
    let log = dir.path().join("args.log");
    let cli = fake().with_env("FAKE_MEMBRANE_ARGS_LOG", &log);
    let repo = dir.path().join("repo");
    let repo_s = repo.to_str().unwrap();
    cli.memory_search(
        "needle",
        &["durable".into(), "session".into()],
        7,
        Some(&repo),
    )
    .unwrap();
    cli.context_pull(dir.path(), "t", None, Some(&repo))
        .unwrap();
    cli.blueprint_search("find me", 4, Some(&repo)).unwrap();
    let logged = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<Vec<&str>> = logged
        .lines()
        .map(|l| l.split('\u{1f}').collect())
        .collect();
    let search = &lines[0];
    assert_eq!(&search[..3], ["memory", "search", "needle"]);
    assert!(search.windows(2).any(|w| w == ["--tier", "durable"]));
    assert!(search.windows(2).any(|w| w == ["--tier", "session"]));
    assert!(search.windows(2).any(|w| w == ["--limit", "7"]));
    assert!(search.windows(2).any(|w| w == ["--repo", repo_s]));
    assert!(search.contains(&"--json"));
    assert!(lines[1].contains(&"--json"));
    assert!(lines[1].windows(2).any(|w| w == ["--repo", repo_s]));
    // `blueprint search --limit n <text> --repo <path>`: the verb leads, `--repo` trails.
    assert_eq!(&lines[2][..4], ["blueprint", "search", "--limit", "4"]);
    assert!(lines[2].windows(2).any(|w| w == ["--repo", repo_s]));
}

#[test]
fn unknown_schema_major_is_rejected() {
    let cli = fake().with_env("FAKE_MEMBRANE_MODE", "v2");
    let dir = scratch();
    let errs = [
        cli.status().map(|_| ()).unwrap_err(),
        cli.memory_list(1, None).map(|_| ()).unwrap_err(),
        cli.memory_search("q", &[], 1, None)
            .map(|_| ())
            .unwrap_err(),
        cli.memory_delete("x").map(|_| ()).unwrap_err(),
        cli.blueprint_search("q", 1, None).map(|_| ()).unwrap_err(),
        cli.adapt_recall("q", 1, "global", &[])
            .map(|_| ())
            .unwrap_err(),
        cli.transcript_tip("s1").map(|_| ()).unwrap_err(),
        cli.context_pull(dir.path(), "t", None, None)
            .map(|_| ())
            .unwrap_err(),
    ];
    for err in errs {
        assert!(
            matches!(err, MembraneError::UnsupportedSchema { .. }),
            "{err:?}"
        );
    }
}

#[test]
fn exit_codes_map_to_errors() {
    let m = |mode: &str| fake().with_env("FAKE_MEMBRANE_MODE", mode);
    assert!(matches!(m("fail").status().unwrap_err(), MembraneError::Internal(r) if r == "boom"));
    assert!(matches!(
        m("usage").status().unwrap_err(),
        MembraneError::Usage(_)
    ));
    assert!(matches!(
        m("unavailable").blueprint_search("q", 1, None).unwrap_err(),
        MembraneError::NotAvailable(r) if r == "repository is not enrolled"
    ));
    assert!(matches!(
        map_exit(Some(4), "membrane: no resident"),
        MembraneError::ResidentUnreachable(_)
    ));
    assert!(matches!(
        map_exit(Some(9), "x"),
        MembraneError::Exit { code: Some(9), .. }
    ));
    // Exit 3 on context pull and memory list means off / no store, not an error.
    let dir = scratch();
    let c = m("unavailable")
        .context_pull(dir.path(), "t", None, None)
        .unwrap();
    assert!(c.text.is_empty());
    assert_eq!(c.off_reason.as_deref(), Some("repository is not enrolled"));
    assert!(m("unavailable")
        .memory_list(3, None)
        .unwrap()
        .items
        .is_empty());
}
