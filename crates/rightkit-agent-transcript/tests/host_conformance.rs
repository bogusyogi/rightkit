use std::path::PathBuf;

use rightkit_agent_transcript::{discover_open, parse_source_events, TranscriptError};

fn unique_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("membrane-host-{label}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn discovery_maps_real_roots() {
    let home = unique_dir("discover");
    for (relative, body) in [
        (".claude/projects/repo/live.jsonl", "{}\n"),
        (".codex/sessions/2026/codex.jsonl", "{}\n"),
    ] {
        let path = home.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let found = discover_open(&home);
    for host in ["claude_code", "codex"] {
        assert!(found.iter().any(|item| item.host == host), "{host}");
    }
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn valid_but_unrecognized_raw_source_is_typed_degradation() {
    let dir = unique_dir("no-events");
    let path = dir.join("raw.jsonl");
    std::fs::write(&path, "{\"type\":\"model_change\",\"model\":\"x\"}\n").unwrap();
    let error = parse_source_events(&path, Some("claude_code")).unwrap_err();
    assert!(matches!(error, TranscriptError::NoEvents { .. }));
    std::fs::remove_dir_all(dir).unwrap();
}
