//! End-to-end transcript ingestion: real host transcript files (committed
//! fixtures) go through the public
//! discovery -> detection -> normalization -> prefix-receipt path exactly as
//! the resident worker's ingestion consumers call it.

use std::path::{Path, PathBuf};

use rightkit_agent_transcript::{
    classify::Classification, detect_host, discover_open, parse, parse_prefix_receipt,
    parse_source_events, TranscriptError,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn claude_and_codex_transcripts_link_tool_calls_and_bind_a_digest_receipt() {
    // Host is auto-detected from the first rows.
    let claude = fixture("claude_code.jsonl");
    assert_eq!(detect_host(&claude).unwrap(), "claude_code");
    assert_eq!(detect_host(&fixture("codex.jsonl")).unwrap(), "codex");
    let events = parse_source_events(&claude, None).unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "thinking",
            "assistant_message",
            "tool_call",
            "tool_result",
            "user_message",
            "meta"
        ]
    );
    let bytes = std::fs::read(&claude).unwrap();
    for event in &events {
        let row = &bytes[event.byte_start as usize..event.byte_end as usize];
        assert_eq!(row.last(), Some(&b'\n'));
        assert_eq!(event.session_id, "sess-claude-1");
    }
    let call = events.iter().find(|e| e.kind == "tool_call").unwrap();
    let result = events.iter().find(|e| e.kind == "tool_result").unwrap();
    assert_eq!(
        result.tool_call_event_id.as_deref(),
        Some(call.event_id.as_str())
    );
    assert_eq!(call.classification(), Classification::Mutation);
    let reasoning = events.iter().find(|e| e.kind == "thinking").unwrap();
    assert_eq!(reasoning.text, "private reasoning omitted");

    // Codex failure output stays linked to its call and flagged.
    let codex = parse_source_events(&fixture("codex.jsonl"), Some("codex")).unwrap();
    let ccall = codex.iter().find(|e| e.kind == "tool_call").unwrap();
    let cout = codex.iter().find(|e| e.kind == "tool_result").unwrap();
    assert_eq!(
        cout.tool_call_event_id.as_deref(),
        Some(ccall.event_id.as_str())
    );
    assert!(cout.text.contains("exit code: 101"));

    // The receipt covers the whole file and is the sha256 of the exact prefix.
    let receipt = parse_prefix_receipt(&claude, None).unwrap().receipt;
    assert_eq!(receipt.prefix_length, bytes.len() as u64);
    assert_eq!(
        receipt.prefix_digest,
        format!("sha256:{}", rightkit_fs::digest::sha256_bytes(&bytes))
    );
    // Deterministic across runs; capped projection keeps the user decision.
    assert_eq!(parse_source_events(&claude, None).unwrap(), events);
}

#[test]
fn secrets_redacted_sidechains_dropped_and_bad_sources_fail_closed() {
    let secrets = parse_source_events(&fixture("secrets_claude.jsonl"), None).unwrap();
    assert!(secrets[0].redacted && secrets[0].flags.redacted);
    assert!(secrets[0].text.contains("[REDACTED]"));
    assert!(!secrets[0].text.contains("sk-abcd1234"));
    let side = parse_source_events(&fixture("sidechain_claude.jsonl"), None).unwrap();
    assert_eq!(side.len(), 1);
    assert_eq!(side[0].text, "visible root message");

    assert!(matches!(
        parse(&fixture("nope.jsonl"), None, None).unwrap_err(),
        TranscriptError::Missing { .. }
    ));
    match parse(&fixture("claude_code.jsonl"), Some("martian"), None).unwrap_err() {
        TranscriptError::UnsupportedHost { host } => assert_eq!(host, "martian"),
        other => panic!("unexpected {other:?}"),
    }

    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty.jsonl");
    std::fs::write(&empty, "").unwrap();
    assert!(matches!(
        parse(&empty, Some("claude_code"), None).unwrap_err(),
        TranscriptError::NoCompleteRow { .. }
    ));

    let first = b"{\"type\":\"user\",\"message\":{\"content\":\"valid\"}}\n";
    let bad = b"{not-json}\n";
    let malformed = dir.path().join("malformed.jsonl");
    std::fs::write(&malformed, [first.as_slice(), bad.as_slice()].concat()).unwrap();
    match parse_source_events(&malformed, Some("claude_code")).unwrap_err() {
        TranscriptError::MalformedRow {
            row_index,
            byte_start,
            byte_end,
            ..
        } => {
            assert_eq!(row_index, 2);
            assert_eq!(byte_start, first.len() as u64);
            assert_eq!(byte_end, (first.len() + bad.len()) as u64);
        }
        other => panic!("unexpected {other:?}"),
    }
    let unrecognized = dir.path().join("raw.jsonl");
    std::fs::write(
        &unrecognized,
        "{\"type\":\"model_change\",\"model\":\"x\"}\n",
    )
    .unwrap();
    assert!(matches!(
        parse_source_events(&unrecognized, Some("claude_code")).unwrap_err(),
        TranscriptError::NoEvents { .. }
    ));
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[test]
fn open_roots_are_discovered() {
    let home = tempfile::tempdir().unwrap();
    for (relative, body) in [
        (".claude/projects/repo/live.jsonl", "{}\n"),
        (".codex/sessions/2026/codex.jsonl", "{}\n"),
    ] {
        write(&home.path().join(relative), body);
    }
    let found = discover_open(home.path());
    for host in ["claude_code", "codex"] {
        assert!(
            found.iter().any(|item| item.host == host),
            "{host} discovered"
        );
    }
}
