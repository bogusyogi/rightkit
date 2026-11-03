//! Artifact and report tool journeys through the REAL `coderight run` binary:
//! artifact_create / artifact_read / artifact_update (optimistic versioning + tamper detection) /
//! artifact_list / artifact_delete (trash), research_report rendering, and the trust-gated
//! attach_verdict / finalize_artifact tools' typed refusal when no trust store is wired.

use crate::support::e2e_tools as t;
use t::*;

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

fn artifact_id(created: &str) -> String {
    json_of(created)["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {created}"))
        .to_string()
}

#[test]
fn artifact_tools_create_read_update_list_delete_with_versioning_and_tamper_detection() {
    let p = sequence(
        vec![
            // 0
            call(
                "artifact_create",
                json!({"path":"reports/summary.md","kind":"markdown","content":"v1 body","provenance":"e2e"}),
            ),
            // 1
            call_with(
                "artifact_read",
                |s| json!({"artifact_id": artifact_id(&s[0])}),
            ),
            // 2 update with the version we saw
            call_with(
                "artifact_update",
                |s| json!({"artifact_id": artifact_id(&s[0]), "content":"v2 body", "expected_version": 1}),
            ),
            // 3 stale expected_version
            call_with(
                "artifact_update",
                |s| json!({"artifact_id": artifact_id(&s[0]), "content":"lost update", "expected_version": 1}),
            ),
            // 4
            call_with(
                "artifact_read",
                |s| json!({"artifact_id": artifact_id(&s[0])}),
            ),
            // 5 a second artifact of another kind
            call(
                "artifact_create",
                json!({"path":"diffs/change.diff","kind":"diff","content":"--- a\n+++ b\n"}),
            ),
            // 6 list all / 7 filter by kind / 8 filter by missing kind
            call("artifact_list", json!({})),
            call("artifact_list", json!({"kind":"diff"})),
            call("artifact_list", json!({"kind":"no-such-kind"})),
            // 9 out-of-band edit of the stored content, then 10 update must refuse to clobber it
            call_with("shell", |s| {
                let id = artifact_id(&s[0]);
                json!({"command": format!("echo tampered > .coderight/artifacts/{id}/content")})
            }),
            call_with(
                "artifact_update",
                |s| json!({"artifact_id": artifact_id(&s[0]), "content":"clobber", "expected_version": 2}),
            ),
            // 11-13 id / path validation
            call("artifact_read", json!({"artifact_id":"../../etc/passwd"})),
            call("artifact_read", json!({"artifact_id":"art_missing"})),
            call(
                "artifact_create",
                json!({"path":"../escape.md","kind":"markdown","content":"x"}),
            ),
            // 14 delete, 15 read after delete, 16 delete again
            call_with(
                "artifact_delete",
                |s| json!({"artifact_id": artifact_id(&s[5])}),
            ),
            call_with(
                "artifact_read",
                |s| json!({"artifact_id": artifact_id(&s[5])}),
            ),
            call_with(
                "artifact_delete",
                |s| json!({"artifact_id": artifact_id(&s[5])}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 17);

    let created = json_of(&r[0].text);
    let id = created["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("art_"), "{created}");
    assert_eq!(created["version"], 1);
    assert_eq!(created["path"], "reports/summary.md");
    assert_eq!(created["kind"], "markdown");
    assert_eq!(created["trustLabel"], "unreviewed");
    assert_eq!(created["shippable"], false);
    assert_eq!(created["provenance"], "e2e");
    assert!(
        created.get("content").is_none(),
        "create must not echo content"
    );

    let read = json_of(&r[1].text);
    assert_eq!(read["content"], "v1 body");
    assert_eq!(read["version"], 1);
    assert_eq!(read["contentHash"], created["contentHash"]);

    let updated = json_of(&r[2].text);
    assert_eq!(updated["version"], 2);
    assert_ne!(updated["contentHash"], created["contentHash"]);

    assert!(r[3].error);
    assert!(
        r[3].text.starts_with(&format!(
            "Version conflict for artifact '{id}': expected_version=1, current version=2."
        )),
        "{}",
        r[3].text
    );
    let read2 = json_of(&r[4].text);
    assert_eq!(
        read2["content"], "v2 body",
        "the conflicting write must not land"
    );
    assert_eq!(read2["version"], 2);

    let diff_id = artifact_id(&r[5].text);
    let listed = json_of(&r[6].text);
    let ids: Vec<&str> = listed["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 2, "{listed}");
    assert!(
        ids.contains(&id.as_str()) && ids.contains(&diff_id.as_str()),
        "{listed}"
    );
    let only_diff = json_of(&r[7].text);
    assert_eq!(only_diff["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(only_diff["artifacts"][0]["id"], diff_id.as_str());
    assert_eq!(
        json_of(&r[8].text)["artifacts"].as_array().unwrap().len(),
        0
    );

    assert!(r[10].error);
    assert!(
        r[10].text.contains(&format!(
            "Artifact '{id}' was modified outside artifact_update"
        )),
        "{}",
        r[10].text
    );
    assert!(r[11].error);
    assert!(
        r[11].text.starts_with("Invalid artifact_id"),
        "{}",
        r[11].text
    );
    assert!(r[12].error);
    assert!(
        r[12].text.starts_with("artifact not found or unreadable"),
        "{}",
        r[12].text
    );
    assert!(r[13].error, "{}", r[13].text);

    assert_eq!(r[14].text, format!("Artifact '{diff_id}' deleted."));
    assert!(r[15].error);
    assert!(
        r[15].text.starts_with("artifact not found or unreadable"),
        "{}",
        r[15].text
    );
    assert!(r[16].error);
    assert_eq!(r[16].text, format!("Artifact not found: {diff_id}"));

    // Durable on-disk shape: content, v1 snapshot kept, deleted artifact moved to trash.
    let dir = format!(".coderight/artifacts/{id}");
    assert!(rig.exists(&format!("{dir}/meta.json")));
    assert_eq!(rig.read(&format!("{dir}/versions/1")), "v1 body");
    assert!(
        !rig.exists(&format!(".coderight/artifacts/{diff_id}")),
        "deleted artifact must leave the live store"
    );
    let trash: Vec<_> = std::fs::read_dir(rig.root().join(".coderight/artifacts_trash"))
        .expect("artifacts_trash exists")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        trash.iter().any(|n| n.starts_with(&diff_id)),
        "deleted artifact must be recoverable from trash: {trash:?}"
    );
}

#[test]
fn research_report_renders_a_cited_markdown_report_and_validates_input() {
    let p = sequence(
        vec![
            call(
                "research_report",
                json!({
                    "title": "  Async runtimes  ",
                    "summary": "Tokio dominates [1].",
                    "sections": [
                        {"heading":"Findings","body":"Tokio is the default runtime [1]. async-std is quieter [2]."},
                        {"heading":"Risks","body":"  Runtime lock-in [1].  "}
                    ],
                    "sources": [
                        {"title":"Tokio docs","url":"https://tokio.rs","note":"official"},
                        {"title":"async-std","url":"https://async.rs"},
                        {"title":"Tokio again","url":"https://tokio.rs"}
                    ]
                }),
            ),
            call(
                "research_report",
                json!({"title":"  ","sections":[],"sources":[]}),
            ),
            call("research_report", json!({"title":"No sections"})),
            call("research_report", json!({"sections":[],"sources":[]})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 4);

    assert!(!r[0].error, "{}", r[0].text);
    assert_eq!(
        r[0].text.trim_end(),
        "# Async runtimes\n\nTokio dominates [1].\n\n## Findings\n\nTokio is the default runtime [1]. async-std is quieter [2].\n\n## Risks\n\nRuntime lock-in [1].\n\n## Sources\n\n1. [Tokio docs](https://tokio.rs) — official\n2. [async-std](https://async.rs)"
    );
    assert!(r[1].error);
    assert_eq!(r[1].text, "research_report needs a non-empty title");
    // `sections` / `sources` are required by the schema; whether the engine or the tool rejects
    // them, the model must be told rather than receive an empty report.
    assert!(
        r[2].error || r[2].text.starts_with("# No sections"),
        "{}",
        r[2].text
    );
    assert!(r[3].error);
}

#[test]
fn trust_gated_artifact_tools_refuse_clearly_when_no_trust_store_is_wired() {
    let p = sequence(
        vec![
            call(
                "attach_verdict",
                json!({"artifact_id":"art1","verdict":"approved","reasoning":"fine","confidence":0.9}),
            ),
            call("finalize_artifact", json!({"artifact_id":"art1"})),
            call("finalize_artifact", json!({})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 3);

    assert!(r[0].error);
    assert!(
        r[0].text
            .starts_with("attach_verdict requires a trust store."),
        "{}",
        r[0].text
    );
    assert!(r[1].error);
    assert!(
        r[1].text
            .starts_with("finalize_artifact requires a trust store."),
        "{}",
        r[1].text
    );
    assert!(r[2].error);
    assert!(r[2].text.contains("artifact_id"), "{}", r[2].text);
}
