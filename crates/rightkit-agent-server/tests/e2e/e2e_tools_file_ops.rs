//! File-mutation journeys through the REAL `coderight run` binary: write / read / edit /
//! multi_edit / apply_patch / undo_edit via FileService, with stale-anchor, conflict and
//! blind-restore refusals, path-boundary errors, image preview and line-ending preservation.
//!
//! Tool results are asserted from the user-visible `toolResultAdded` stream events; file effects
//! are asserted on disk in the session's execution root (`Rig::root()`).
//! Retires the unit tests of `write_file`, `edit_file`, `multi_edit`, `undo_edit`,
//! `apply_patch`, `view_image`, `file_service`, `file_version_guard`, `observed_version`,
//! `edit_helpers` and the `read_file` readers.

use crate::support::e2e_tools as t;
use t::*;

/// Platform-neutral view of a path-bearing message.
fn norm(s: &str) -> String {
    s.replace('\\', "/")
}

#[test]
fn write_read_edit_multi_edit_roundtrip() {
    let p = sequence(
        vec![
            call(
                "write_file",
                json!({"path":"src/a.txt","content":"one\ntwo\nthree\n"}),
            ),
            call("read_file", json!({"path":"src/a.txt"})),
            call(
                "edit_file",
                json!({"path":"src/a.txt","old_string":"two","new_string":"TWO"}),
            ),
            call("read_file", json!({"path":"src/a.txt"})),
            call(
                "multi_edit",
                json!({"path":"src/a.txt","edits":[
                    {"old_string":"one","new_string":"1"},
                    {"old_string":"three","new_string":"3"}]}),
            ),
        ],
        "all done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 5);
    assert!(r.iter().all(|x| !x.error), "no step may fail: {r:#?}");

    let created = norm(&r[0].text);
    assert!(created.starts_with("Created '"), "{created}");
    assert!(
        created.contains("src/a.txt' (14 bytes, 3 lines)."),
        "{created}"
    );

    // read_file renders `{seq:>6}:{hash:03x}: {line}`.
    let read: Vec<&str> = r[1].text.lines().collect();
    assert_eq!(read.len(), 3, "{read:?}");
    for (i, want) in ["one", "two", "three"].iter().enumerate() {
        let line = read[i];
        assert!(line.starts_with(&format!("{:>6}:", i + 1)), "{line:?}");
        assert!(line.ends_with(&format!(": {want}")), "{line:?}");
        assert_eq!(line.len(), 6 + 1 + 3 + 2 + want.len(), "{line:?}");
    }

    assert!(r[2].text.starts_with("Edited '"), "{}", r[2].text);
    assert!(
        r[2].text
            .contains("': replaced 1 occurrence (14 → 14 bytes)."),
        "{}",
        r[2].text
    );
    assert!(r[2].text.contains("```diff"), "{}", r[2].text);
    assert!(r[4].text.starts_with("Edited '"), "{}", r[4].text);
    assert!(
        r[4].text.contains("': applied 2 edit(s) (14 → 8 bytes)."),
        "{}",
        r[4].text
    );

    assert_eq!(rig.read("src/a.txt"), "1\nTWO\n3\n");
}

#[test]
fn unread_or_stale_files_are_refused_and_external_changes_survive() {
    let cell = RootCell::new();
    let cell_for_script = cell.clone();
    let p = sequence(
        vec![
            // 0: never read -> refused.
            call(
                "edit_file",
                json!({"path":"notes.txt","old_string":"alpha","new_string":"ALPHA"}),
            ),
            // 1: blind overwrite of an existing file -> refused.
            call(
                "write_file",
                json!({"path":"notes.txt","content":"blind\n"}),
            ),
            // 2: read, then someone else changes the file behind our back, then edit -> conflict.
            call("read_file", json!({"path":"notes.txt"})),
            call_with("edit_file", move |_| {
                std::fs::write(
                    cell_for_script.root().join("notes.txt"),
                    "alpha\nbeta\nEXTERNAL-CHANGE\n",
                )
                .unwrap();
                json!({"path":"notes.txt","old_string":"alpha","new_string":"ALPHA"})
            }),
            // 4: the stale receipt also blocks a full overwrite.
            call(
                "write_file",
                json!({"path":"notes.txt","content":"clobber\n"}),
            ),
            // 5-6: re-read, retry -> succeeds on top of the external change.
            call("read_file", json!({"path":"notes.txt"})),
            call(
                "edit_file",
                json!({"path":"notes.txt","old_string":"alpha","new_string":"ALPHA"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[("notes.txt", "alpha\nbeta\n")]);
    cell.bind(&rig);
    let out = rig.run("go");
    let r = expect_results(&out, 7);

    for (i, tool) in [(0usize, "edit"), (1, "write")] {
        assert!(r[i].error, "{tool} without a read must fail: {}", r[i].text);
        assert!(
            r[i].text.contains("Refusing mutation of '"),
            "{}",
            r[i].text
        );
        assert!(
            r[i].text
                .contains("': read the complete current file first, then retry."),
            "{}",
            r[i].text
        );
    }
    assert!(r[0].text.starts_with("Failed to write '"), "{}", r[0].text);
    assert!(
        r[1].text.starts_with("Failed to write file '"),
        "{}",
        r[1].text
    );

    assert!(!r[2].error);
    assert!(r[3].error, "{}", r[3].text);
    assert!(r[3].text.starts_with("Failed to write '"), "{}", r[3].text);
    assert!(
        r[3].text.contains("mutation conflict (Update) for '"),
        "{}",
        r[3].text
    );

    assert!(r[4].error, "{}", r[4].text);
    assert!(
        r[4].text.contains("mutation conflict (Replace) for '"),
        "{}",
        r[4].text
    );

    assert!(!r[5].error);
    assert!(!r[6].error, "{}", r[6].text);
    assert!(r[6].text.starts_with("Edited '"), "{}", r[6].text);

    // The external change was never clobbered; the retried edit landed on top of it.
    assert_eq!(rig.read("notes.txt"), "ALPHA\nbeta\nEXTERNAL-CHANGE\n");
}

#[test]
fn undo_restores_the_snapshot_but_never_blindly_overwrites_newer_bytes() {
    let cell = RootCell::new();
    let cell_for_script = cell.clone();
    let p = sequence(
        vec![
            // 0: nothing to undo yet.
            call("undo_edit", json!({"path":"u.txt"})),
            // 1-3: edit then undo -> back to the original bytes.
            call("read_file", json!({"path":"u.txt"})),
            call(
                "edit_file",
                json!({"path":"u.txt","old_string":"v0","new_string":"v1"}),
            ),
            call("undo_edit", json!({"path":"u.txt"})),
            // 4-6: edit again, someone else rewrites the file, undo must refuse.
            call("read_file", json!({"path":"u.txt"})),
            call(
                "edit_file",
                json!({"path":"u.txt","old_string":"v0","new_string":"v2"}),
            ),
            call_with("undo_edit", move |_| {
                std::fs::write(
                    cell_for_script.root().join("u.txt"),
                    "changed-by-someone-else\n",
                )
                .unwrap();
                json!({"path":"u.txt"})
            }),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[("u.txt", "v0\n")]);
    cell.bind(&rig);
    let out = rig.run("go");
    let r = expect_results(&out, 7);

    assert!(r[0].error);
    assert!(
        r[0].text.starts_with("No edit history for '"),
        "{}",
        r[0].text
    );
    assert!(
        r[0].text.ends_with("u.txt'. Nothing to undo."),
        "{}",
        r[0].text
    );

    assert!(!r[2].error, "{}", r[2].text);
    assert!(!r[3].error, "{}", r[3].text);
    assert!(r[3].text.starts_with("Reverted '"), "{}", r[3].text);
    assert!(
        r[3].text.contains("' to previous snapshot (3 bytes)."),
        "{}",
        r[3].text
    );

    assert!(!r[5].error, "{}", r[5].text);
    assert!(r[6].error, "{}", r[6].text);
    assert!(r[6].text.starts_with("Refusing to undo '"), "{}", r[6].text);
    assert!(
        r[6].text
            .ends_with("': file changed after the agent wrote it; newer bytes were preserved."),
        "{}",
        r[6].text
    );
    assert_eq!(rig.read("u.txt"), "changed-by-someone-else\n");
}

#[test]
fn multi_edit_is_atomic_and_ambiguous_edits_fail_closed() {
    let p = sequence(
        vec![
            call("read_file", json!({"path":"m.txt"})),
            // second edit cannot match -> NOTHING is written, not even the first edit.
            call(
                "multi_edit",
                json!({"path":"m.txt","edits":[
                    {"old_string":"a","new_string":"A"},
                    {"old_string":"zzz","new_string":"Z"}]}),
            ),
            call("read_file", json!({"path":"dup.txt"})),
            call(
                "edit_file",
                json!({"path":"dup.txt","old_string":"dup","new_string":"x"}),
            ),
            call(
                "edit_file",
                json!({"path":"dup.txt","old_string":"dup","new_string":"dup2","replace_all":true}),
            ),
            call(
                "edit_file",
                json!({"path":"dup.txt","old_string":"same","new_string":"same"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[("m.txt", "a\nb\nc\n"), ("dup.txt", "dup\ndup\n")]);
    let out = rig.run("go");
    let r = expect_results(&out, 6);

    assert!(r[1].error);
    assert!(
        r[1].text.contains(
            "edits[1] (after applying 1 earlier edit(s)); no changes written: old_string not found."
        ),
        "{}",
        r[1].text
    );
    assert_eq!(
        rig.read("m.txt"),
        "a\nb\nc\n",
        "failed multi_edit must not write anything"
    );

    assert!(r[3].error);
    assert!(
        r[3].text.contains(
            "old_string is not unique (2 matches). Add surrounding context or set replace_all=true."
        ),
        "{}",
        r[3].text
    );
    assert!(!r[4].error, "{}", r[4].text);
    assert!(
        r[4].text.contains("': replaced 2 occurrences ("),
        "{}",
        r[4].text
    );
    assert_eq!(rig.read("dup.txt"), "dup2\ndup2\n");

    assert!(r[5].error);
    assert_eq!(
        r[5].text,
        "old_string and new_string are identical — nothing to change."
    );
}

#[test]
fn apply_patch_applies_creates_and_fails_closed_across_files() {
    let p = sequence(
        vec![
            call(
                "apply_patch",
                json!({"patch":"*** p.txt\none\n--- two\n+++ TWO\nthree\n"}),
            ),
            // Second hunk cannot match: the first file must stay untouched too.
            call(
                "apply_patch",
                json!({"patch":"*** q.txt\n--- x\n+++ y\n*** p.txt\n--- nonexistent\n+++ z\n"}),
            ),
            call("apply_patch", json!({"patch":"*** new/n.txt\n+++ hello\n"})),
            call("apply_patch", json!({"patch":"no file header here"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[("p.txt", "one\ntwo\nthree\n"), ("q.txt", "x\n")]);
    let out = rig.run("go");
    let r = expect_results(&out, 4);

    assert!(!r[0].error, "{}", r[0].text);
    assert!(r[0].text.starts_with("Patch applied to: "), "{}", r[0].text);
    assert!(norm(&r[0].text).ends_with("p.txt"), "{}", r[0].text);

    assert!(r[1].error);
    assert!(
        r[1].text.starts_with("Failed to prepare patch for '"),
        "{}",
        r[1].text
    );
    assert!(
        r[1].text
            .contains("Line to remove not found starting at line 1: expected: \"nonexistent\""),
        "{}",
        r[1].text
    );
    assert_eq!(
        rig.read("q.txt"),
        "x\n",
        "a failed multi-file patch must not partially apply"
    );

    assert!(!r[2].error, "{}", r[2].text);
    assert_eq!(rig.read("p.txt"), "one\nTWO\nthree\n");
    assert_eq!(rig.read("new/n.txt"), "hello");

    assert!(r[3].error);
    assert_eq!(
        r[3].text,
        "No file paths found in patch (lines must start with '*** ')"
    );
}

#[test]
fn paths_outside_the_workspace_and_directories_are_rejected() {
    let abs_outside = std::env::temp_dir().join("coderight-e2e-abs-escape.txt");
    let abs_outside_text = abs_outside.to_string_lossy().to_string();
    let p = sequence(
        vec![
            call("write_file", json!({"path":"../escape.txt","content":"x"})),
            call(
                "write_file",
                json!({"path": abs_outside_text, "content":"x"}),
            ),
            call("read_file", json!({"path":"src"})),
            call("read_file", json!({"path":"missing.txt"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[("src/a.txt", "x\n")]);
    let out = rig.run("go");
    let r = expect_results(&out, 4);

    assert!(r[0].error);
    assert!(
        r[0].text
            .starts_with("Path '../escape.txt' escapes the workspace root '"),
        "{}",
        r[0].text
    );
    assert!(!rig.root().join("../escape.txt").exists());

    assert!(r[1].error);
    assert!(
        r[1].text.contains("escapes the workspace root '"),
        "{}",
        r[1].text
    );
    assert!(
        !abs_outside.exists(),
        "write outside the workspace must never happen"
    );

    assert!(r[2].error);
    assert!(
        r[2].text
            .ends_with("' is a directory, not a file. Use list_files to see its contents."),
        "{}",
        r[2].text
    );
    assert!(r[3].error);
    assert!(
        r[3].text.starts_with("Failed to read file '"),
        "{}",
        r[3].text
    );
}

#[test]
fn view_image_describes_real_images_and_rejects_mislabeled_ones() {
    let p = sequence(
        vec![
            call("view_image", json!({"path":"pic.png"})),
            call("view_image", json!({"path":"fake.png"})),
            call("view_image", json!({"path":"notes.txt"})),
        ],
        "done",
    );
    let rig = Rig::new(
        &p,
        &[("fake.png", "this is not a png"), ("notes.txt", "hi\n")],
    );
    rig.seed_bytes("pic.png", TINY_PNG);
    let out = rig.run("go");
    let r = expect_results(&out, 3);

    assert!(!r[0].error, "{}", r[0].text);
    assert!(
        r[0].text.starts_with("[image-block]\npath: "),
        "{}",
        r[0].text
    );
    assert!(
        r[0].text.contains(&format!(
            "\nsize: {} bytes\nmime: image/png\ndimensions: 1x1\ndata_url: data:image/png;base64,",
            TINY_PNG.len()
        )),
        "{}",
        r[0].text
    );

    assert!(r[1].error);
    assert!(
        r[1].text.contains(
            "is unsupported for image preview: extension is .png but the file does not start with a PNG signature."
        ),
        "{}",
        r[1].text
    );
    assert!(r[2].error);
    assert!(
        r[2].text
            .contains("Extension 'txt' is not a recognized image format."),
        "{}",
        r[2].text
    );
}

#[test]
fn overwriting_a_crlf_file_keeps_its_line_endings() {
    let p = sequence(
        vec![
            call("read_file", json!({"path":"crlf.txt"})),
            call("write_file", json!({"path":"crlf.txt","content":"x\ny\n"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[("crlf.txt", "a\r\nb\r\n")]);
    let out = rig.run("go");
    let r = expect_results(&out, 2);
    assert!(!r[1].error, "{}", r[1].text);
    assert!(r[1].text.starts_with("Overwrote '"), "{}", r[1].text);
    assert!(
        r[1].text.contains("crlf.txt' (6 bytes, 2 lines)."),
        "{}",
        norm(&r[1].text)
    );
    assert_eq!(rig.read_bytes("crlf.txt").unwrap(), b"x\r\ny\r\n".to_vec());
}

// A066: the post-edit gate re-enters the turn with the failing check instead of letting the
// model finish, and the turn only ends once the edited bytes pass the check again.
#[test]
fn post_edit_check_gate_blocks_turn_end_until_checks_pass() {
    const MARKER: &str = "no longer valid JSON";
    let p = Brainy::start(|req, _| {
        let results = tool_results(req);
        let gate_fired = req.to_string().contains(MARKER);
        match results.len() {
            0 => Chat::tool(
                "w1",
                "write_file",
                json!({"path":"cfg.json","content":"{\"a\": "}),
            ),
            // After the broken write the gate must have injected the failing check. A model that
            // tried to finish here would be told so; this one only proceeds when the gate fired.
            1 if gate_fired => Chat::tool("r1", "read_file", json!({"path":"cfg.json"})),
            1 => Chat::text("finished-without-gate"),
            2 => Chat::tool(
                "e1",
                "edit_file",
                json!({"path":"cfg.json","old_string":"{\"a\": ","new_string":"{\"a\": 1}"}),
            ),
            _ => Chat::text("repaired"),
        }
    });
    let rig = Rig::new(&p, &[]);
    let out = rig.run("write cfg.json");
    assert_eq!(out.code, Some(0), "{}", out.diag());

    let requests = p.requests();
    assert!(
        requests.len() >= 2 && requests[1].to_string().contains(MARKER),
        "the request right after the broken write must carry the failing post-edit check: {:?}",
        requests.get(1)
    );
    assert!(
        !out.stdout.contains("finished-without-gate"),
        "the turn must not end while the edited file fails its check: {}",
        out.diag()
    );
    let body = rig.read("cfg.json");
    assert!(
        serde_json::from_str::<serde_json::Value>(&body).is_ok(),
        "the file must parse again once the turn ends: {body:?}"
    );
}
