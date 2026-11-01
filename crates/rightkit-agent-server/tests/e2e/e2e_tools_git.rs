//! Git and checkpoint journeys through the REAL `coderight run` binary: the guarded `git_write`
//! init / stage / commit / branch flow (verified with the real `git` CLI afterwards) and
//! `workspace_checkpoint` create / list / restore round-trips that never move HEAD and never
//! delete ignored session state.
//! Retires the unit tests of `git_read`, `git_write`, `git_diff`, `workspace_checkpoint`.

use crate::support::e2e_tools as t;
use t::*;

#[test]
fn git_write_init_stage_commit_and_branch_are_guarded_and_real() {
    let p = sequence(
        vec![
            call("git_write", json!({"operation":"init"})),
            call("write_file", json!({"path":"hello.txt","content":"hi\n"})),
            // Nothing staged yet.
            call("git_write", json!({"operation":"commit","message":"empty"})),
            call(
                "git_write",
                json!({"operation":"stage","paths":["hello.txt"]}),
            ),
            call(
                "git_write",
                json!({"operation":"commit","message":"add hello"}),
            ),
            // Index now equals HEAD.
            call("git_write", json!({"operation":"commit","message":"again"})),
            call(
                "git_write",
                json!({"operation":"branch_create","branch":"feature-x"}),
            ),
        ],
        "git done",
    );
    let rig = Rig::new_unversioned(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 7);

    let init: Value = serde_json::from_str(&r[0].text).expect("init returns JSON");
    assert_eq!(init["operation"], "init");
    assert!(
        rig.root().join(".git").exists(),
        "git_write init must create the repository"
    );

    assert!(r[2].error);
    assert_eq!(
        r[2].text,
        "Refusing empty commit: the index has no entries."
    );

    let staged: Value = serde_json::from_str(&r[3].text).expect("stage returns JSON");
    assert_eq!(staged["operation"], "stage");
    assert_eq!(staged["effect"]["paths"], json!(["hello.txt"]));
    assert!(
        staged["effect"]["diff"].as_str().unwrap().contains("+hi"),
        "{staged}"
    );

    let committed: Value = serde_json::from_str(&r[4].text).expect("commit returns JSON");
    assert_eq!(committed["operation"], "commit");
    assert_eq!(committed["effect"]["message"], "add hello");
    let sha = committed["effect"]["commit"].as_str().unwrap().to_string();
    assert_eq!(sha.len(), 40, "{sha}");
    assert_eq!(rig.git_out(&["rev-parse", "HEAD"]), sha);
    assert_eq!(rig.git_out(&["log", "--format=%s"]), "add hello");
    assert_eq!(rig.git_out(&["show", "HEAD:hello.txt"]), "hi");

    assert!(r[5].error);
    assert_eq!(r[5].text, "Refusing empty commit: index matches HEAD.");

    let branch: Value = serde_json::from_str(&r[6].text).expect("branch_create returns JSON");
    assert_eq!(branch["effect"]["branch"], "feature-x");
    assert_eq!(branch["effect"]["target"], sha.as_str());
    assert!(rig
        .git_out(&["branch", "--list", "feature-x"])
        .contains("feature-x"));
}

#[test]
fn checkpoint_create_restore_round_trips_files_and_keeps_ignored_state() {
    let p = sequence(
        vec![
            call("git_write", json!({"operation":"init"})),
            call(
                "workspace_checkpoint",
                json!({"operation":"create","label":"cp1"}),
            ),
            call("read_file", json!({"path":"a.txt"})),
            call(
                "edit_file",
                json!({"path":"a.txt","old_string":"v1","new_string":"v2"}),
            ),
            call("write_file", json!({"path":"b.txt","content":"new\n"})),
            call("workspace_checkpoint", json!({"operation":"list"})),
            call_with(
                "workspace_checkpoint",
                |seen| json!({"operation":"restore","checkpoint_id": checkpoint_id(&seen[1])}),
            ),
            call(
                "workspace_checkpoint",
                json!({"operation":"restore","checkpoint_id":"deadbeef"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new_unversioned(&p, &[(".gitignore", ".coderight/\n"), ("a.txt", "v1\n")]);
    let out = rig.run("go");
    let r = expect_results(&out, 8);

    let id = checkpoint_id(&r[1].text);
    assert_eq!(id.len(), 40, "{}", r[1].text);
    assert!(
        r[1].text
            .starts_with(&format!("Checkpoint created: {id} (tree ")),
        "{}",
        r[1].text
    );

    let listed: Value = serde_json::from_str(&r[5].text).expect("list returns JSON");
    let listed = listed.as_array().expect("array");
    assert_eq!(listed.len(), 1, "{}", r[5].text);
    assert_eq!(listed[0]["id"], id.as_str());
    assert_eq!(listed[0]["commit_oid"], id.as_str());
    assert_eq!(listed[0]["label"], "cp1");

    assert!(!r[6].error, "{}", r[6].text);
    let restored: Value = serde_json::from_str(&r[6].text).expect("restore returns JSON");
    assert!(
        restored["message"]
            .as_str()
            .unwrap()
            .starts_with(&format!("Restored checkpoint {id} (commit ")),
        "{restored}"
    );

    // The checkpoint is a real, detached commit object; no branch was created or moved.
    assert_eq!(rig.git_out(&["cat-file", "-t", &id]), "commit");
    assert_eq!(rig.git_out(&["branch", "--list"]), "");

    // Files are back to the checkpoint; the file created afterwards is gone; ignored session
    // state (`.coderight/`, which holds the checkpoint records themselves) survived.
    assert_eq!(rig.read("a.txt"), "v1\n");
    assert!(
        !rig.exists("b.txt"),
        "files created after the checkpoint must be removed"
    );
    assert!(rig.root().join(".coderight").join("checkpoints").exists());

    assert!(r[7].error);
    assert_eq!(r[7].text, "Unknown checkpoint_id: deadbeef");
}

/// `Checkpoint created: <id> (tree <oid>)` -> `<id>`.
fn checkpoint_id(text: &str) -> String {
    let marker = "Checkpoint created: ";
    let start = text.find(marker).expect("create result") + marker.len();
    text[start..]
        .split_whitespace()
        .next()
        .expect("checkpoint id")
        .to_string()
}

// ───────────────────────── typed git tools on a real repository ───────────────

fn shell(cmd: &str) -> Step {
    call("shell", json!({ "command": cmd }))
}

/// Portable "print a file" for the `shell` tool.
fn cat_cmd(path: &str) -> String {
    if cfg!(windows) {
        format!("Get-Content {path}")
    } else {
        format!("cat {path}")
    }
}

/// CG-B02 confines Git away from the host's global configuration. Seed its
/// commit identity and line endings inside the fixture repository itself.
const INIT: &str = "git init -q -b main; git config core.autocrlf false; git config user.name e2e; git config user.email e2e@example.test";

/// Collects steps and hands back the result index of each one.
#[derive(Default)]
struct Script {
    steps: Vec<Step>,
}

impl Script {
    fn push(&mut self, step: Step) -> usize {
        self.steps.push(step);
        self.steps.len() - 1
    }
    fn tool(&mut self, name: &'static str, args: Value) -> usize {
        self.push(call(name, args))
    }
    fn shell(&mut self, cmd: &str) -> usize {
        self.push(shell(cmd))
    }
    /// read_file then edit_file; returns the edit's index.
    fn edit(&mut self, path: &'static str, old: &'static str, new: &'static str) -> usize {
        self.tool("read_file", json!({ "path": path }));
        self.tool(
            "edit_file",
            json!({ "path": path, "old_string": old, "new_string": new }),
        )
    }
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

fn expect_git_results(rig: &Rig, out: &Out, total: usize) -> Vec<Res> {
    // INIT is step zero in these journeys. Report its complete result before
    // a downstream JSON assertion can hide a failed sandboxed initialization.
    let observed = results(out);
    let init = observed.first();
    assert!(
        init.is_some_and(|result| !result.error) && rig.root().join(".git/HEAD").is_file(),
        "sandboxed INIT did not create a repository at {}\nINIT: {INIT}\nINIT result: {init:#?}\n{}",
        rig.root().display(),
        out.diag()
    );
    expect_results(out, total)
}

#[test]
fn git_read_and_git_diff_report_real_repository_state() {
    let mut s = Script::default();
    s.shell(INIT);
    s.tool("write_file", json!({"path":"a.txt","content":"v1\n"}));
    s.shell("git add a.txt; git commit -q -m first");
    let first_sha = s.shell("git rev-parse HEAD");
    s.edit("a.txt", "v1", "v2");
    s.shell("git commit -q -am second");
    let head_sha = s.shell("git rev-parse HEAD");
    s.tool(
        "write_file",
        json!({"path":"new.txt","content":"untracked\n"}),
    );
    let summary = s.tool("git_read", json!({"operation":"summary"}));
    let status = s.tool("git_read", json!({"operation":"status"}));
    let log_one = s.tool("git_read", json!({"operation":"log","max_count":1}));
    let log_all = s.tool("git_read", json!({"operation":"log"}));
    let show_blob = s.tool(
        "git_read",
        json!({"operation":"show","ref":"HEAD~1","path":"a.txt"}),
    );
    let show_commit = s.tool("git_read", json!({"operation":"show"}));
    let blame = s.tool("git_read", json!({"operation":"blame","path":"a.txt"}));
    let branch = s.tool("git_read", json!({"operation":"branch"}));
    s.shell("git remote add origin https://user:topsecretvalue@example.invalid/x.git");
    let discover = s.tool("git_read", json!({"operation":"discover"}));
    let remotes = s.tool("git_read", json!({"operation":"remotes"}));
    let escape = s.tool(
        "git_read",
        json!({"operation":"show","ref":"HEAD","path":"../escape.txt"}),
    );
    let bad_ref = s.tool("git_read", json!({"operation":"log","ref":"no-such-ref"}));
    // git_diff on an unstaged edit, staged edit, bad inputs, then a clean path.
    s.edit("a.txt", "v2", "v3");
    let diff_all = s.tool("git_diff", json!({}));
    let diff_path = s.tool("git_diff", json!({"paths":["a.txt"]}));
    s.shell("git add a.txt");
    let diff_staged = s.tool("git_diff", json!({"staged":true}));
    let diff_empty_paths = s.tool("git_diff", json!({"paths":[]}));
    let diff_bad_base = s.tool("git_diff", json!({"base":"no-such-ref"}));
    s.shell("git commit -q -am third");
    let diff_clean = s.tool("git_diff", json!({"paths":["a.txt"]}));
    let total = s.steps.len();

    let p = sequence(s.steps, "done");
    let rig = Rig::new_unversioned(&p, &[]);
    let out = rig.run_with_deadline("go", 300);
    let r = expect_git_results(&rig, &out, total);

    let first = r[first_sha].text.trim().to_string();
    let head = r[head_sha].text.trim().to_string();
    assert_eq!(first.len(), 40, "{first}");
    assert_eq!(head.len(), 40, "{head}");

    assert!(
        r[summary].text.starts_with("Branch: main\n"),
        "{}",
        r[summary].text
    );
    assert!(
        r[summary].text.contains("Recent commits:"),
        "{}",
        r[summary].text
    );
    assert!(r[summary].text.contains(&head[..7]), "{}", r[summary].text);
    assert!(r[summary].text.contains(" second"), "{}", r[summary].text);
    assert!(r[summary].text.contains(" first"), "{}", r[summary].text);
    assert!(r[summary].text.contains("new.txt"), "{}", r[summary].text);

    let st = json_of(&r[status].text);
    assert!(
        st["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "new.txt"),
        "{st}"
    );

    let one = json_of(&r[log_one].text);
    assert_eq!(one["commits"].as_array().unwrap().len(), 1, "{one}");
    assert_eq!(one["commits"][0]["id"], head.as_str());
    assert_eq!(one["commits"][0]["summary"], "second");
    let all = json_of(&r[log_all].text);
    let ids: Vec<&str> = all["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![head.as_str(), first.as_str()]);

    assert_eq!(r[show_blob].text, "v1\n");
    let shown = json_of(&r[show_commit].text);
    assert_eq!(shown["id"], head.as_str());
    assert_eq!(shown["summary"], "second");
    assert_eq!(shown["parents"], json!([first]));

    let blamed = json_of(&r[blame].text);
    assert_eq!(blamed["path"], "a.txt");
    assert_eq!(blamed["hunks"][0]["commit"], head.as_str());

    let branches = json_of(&r[branch].text);
    let main = branches["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "main")
        .unwrap_or_else(|| panic!("{branches}"));
    assert_eq!(main["head"], true);
    assert_eq!(main["kind"], "local");
    assert_eq!(main["target"], head.as_str());

    let disc = json_of(&r[discover].text);
    assert_eq!(disc["headBranch"], "main", "{disc}");
    assert_eq!(disc["isBare"], false);
    assert_eq!(disc["remotes"].as_array().unwrap().len(), 1, "{disc}");

    let rem = json_of(&r[remotes].text);
    assert_eq!(rem["remotes"][0]["name"], "origin");
    let url = rem["remotes"][0]["url"].as_str().unwrap();
    assert!(url.contains("example.invalid/x.git"), "{url}");
    assert!(
        !r[remotes].text.contains("topsecretvalue"),
        "credentials must be masked before the model sees them: {}",
        r[remotes].text
    );

    assert!(r[escape].error, "{}", r[escape].text);
    assert!(r[bad_ref].error);
    assert!(
        r[bad_ref].text.contains("Unknown ref 'no-such-ref'"),
        "{}",
        r[bad_ref].text
    );

    assert!(
        r[diff_all].text.contains("-v2\n+v3\n"),
        "{}",
        r[diff_all].text
    );
    assert!(
        r[diff_all].text.contains("untracked"),
        "untracked content listed: {}",
        r[diff_all].text
    );
    assert!(
        r[diff_path].text.contains("-v2\n+v3\n"),
        "{}",
        r[diff_path].text
    );
    assert!(
        !r[diff_path].text.contains("untracked"),
        "{}",
        r[diff_path].text
    );
    assert!(
        r[diff_staged].text.contains("-v2\n+v3\n"),
        "{}",
        r[diff_staged].text
    );
    assert!(r[diff_empty_paths].error);
    assert_eq!(
        r[diff_empty_paths].text,
        "'paths' must not be empty when supplied."
    );
    assert!(r[diff_bad_base].error);
    assert!(
        r[diff_bad_base]
            .text
            .starts_with("Failed to resolve base ref 'no-such-ref'"),
        "{}",
        r[diff_bad_base].text
    );
    assert_eq!(r[diff_clean].text, "(no changes)");
    // Read-only tools changed nothing.
    assert_eq!(rig.git_out(&["log", "--format=%s"]), "third\nsecond\nfirst");
}

#[test]
fn git_history_ops_stash_tag_history_detail_and_revert_are_real() {
    let mut s = Script::default();
    s.shell(INIT);
    s.tool("write_file", json!({"path":"a.txt","content":"v1\n"}));
    s.shell("git add a.txt; git commit -q -m first");
    s.edit("a.txt", "v1", "v2");
    let save = s.tool(
        "git_history_ops",
        json!({"operation":"stash_save","message":"wip"}),
    );
    let after_save = s.shell(&cat_cmd("a.txt"));
    let list = s.tool("git_history_ops", json!({"operation":"stash_list"}));
    let preview = s.tool(
        "git_history_ops",
        json!({"operation":"stash_apply","index":0,"preview":true}),
    );
    let pop = s.tool(
        "git_history_ops",
        json!({"operation":"stash_pop","index":0}),
    );
    let after_pop = s.shell(&cat_cmd("a.txt"));
    let list_empty = s.tool("git_history_ops", json!({"operation":"stash_list"}));
    let drop_missing = s.tool(
        "git_history_ops",
        json!({"operation":"stash_drop","index":0}),
    );
    let no_index = s.tool("git_history_ops", json!({"operation":"stash_pop"}));
    s.shell("git commit -q -am second");
    let tag = s.tool(
        "git_history_ops",
        json!({"operation":"tag_create","tag":"v0.1","annotated_message":"release one"}),
    );
    let tag_dup = s.tool(
        "git_history_ops",
        json!({"operation":"tag_create","tag":"v0.1"}),
    );
    let tag_bad = s.tool(
        "git_history_ops",
        json!({"operation":"tag_create","tag":"bad..name"}),
    );
    let hist = s.tool("git_history_ops", json!({"operation":"history","limit":5}));
    let hist_skip = s.tool(
        "git_history_ops",
        json!({"operation":"history","skip":1,"limit":5}),
    );
    let detail = s.tool(
        "git_history_ops",
        json!({"operation":"commit_detail","commit":"HEAD"}),
    );
    let revert = s.tool(
        "git_history_ops",
        json!({"operation":"revert","commit":"HEAD"}),
    );
    let after_revert = s.shell(&cat_cmd("a.txt"));
    let tag_del = s.tool(
        "git_history_ops",
        json!({"operation":"tag_delete","tag":"v0.1"}),
    );
    let total = s.steps.len();

    let p = sequence(s.steps, "done");
    let rig = Rig::new_unversioned(&p, &[]);
    let out = rig.run("go");
    let r = expect_git_results(&rig, &out, total);

    let saved = json_of(&r[save].text);
    assert_eq!(saved["operation"], "stash_save");
    assert_eq!(
        saved["effect"]["stash"].as_str().unwrap().len(),
        40,
        "{saved}"
    );
    assert_eq!(
        r[after_save].text.trim(),
        "v1",
        "stash must restore the tree to HEAD"
    );

    let listed = json_of(&r[list].text);
    let stashes = listed["effect"]["stashes"].as_array().unwrap();
    assert_eq!(stashes.len(), 1, "{listed}");
    assert!(
        stashes[0]["message"].as_str().unwrap().contains("wip"),
        "{listed}"
    );
    assert_eq!(stashes[0]["oid"], saved["effect"]["stash"]);

    let prev = json_of(&r[preview].text);
    assert_eq!(prev["effect"]["preview"], true);
    assert_eq!(prev["effect"]["applied"], false);

    let popped = json_of(&r[pop].text);
    assert_eq!(popped["effect"]["applied"], true);
    assert_eq!(popped["effect"]["dropped"], true);
    assert_eq!(
        r[after_pop].text.trim(),
        "v2",
        "pop must re-apply the stashed edit"
    );
    assert_eq!(
        json_of(&r[list_empty].text)["effect"]["stashes"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(r[drop_missing].error);
    assert!(
        r[drop_missing].text.starts_with("Stash drop [0]:"),
        "{}",
        r[drop_missing].text
    );
    assert!(r[no_index].error);
    assert_eq!(
        r[no_index].text,
        "git_history_ops requires an exact non-negative integer 'index'."
    );

    let created = json_of(&r[tag].text);
    assert_eq!(created["effect"]["tag"], "v0.1");
    assert_eq!(
        created["effect"]["target"],
        rig.git_out(&["rev-parse", "HEAD"]).as_str()
    );
    assert!(r[tag_dup].error);
    assert!(
        r[tag_dup].text.contains("already exists"),
        "{}",
        r[tag_dup].text
    );
    assert!(r[tag_bad].error);
    assert_eq!(r[tag_bad].text, "Invalid tag name 'bad..name'.");

    let h = json_of(&r[hist].text);
    let summaries: Vec<&str> = h["effect"]["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["summary"].as_str().unwrap())
        .collect();
    assert_eq!(summaries, vec!["second", "first"]);
    let h2 = json_of(&r[hist_skip].text);
    assert_eq!(h2["effect"]["commits"].as_array().unwrap().len(), 1);
    assert_eq!(h2["effect"]["commits"][0]["summary"], "first");

    let d = json_of(&r[detail].text);
    assert_eq!(d["effect"]["summary"], "second");
    assert_eq!(d["effect"]["files"], json!(["a.txt"]));

    let rev = json_of(&r[revert].text);
    assert_eq!(rev["effect"]["status"], "reverted");
    assert_eq!(
        r[after_revert].text.trim(),
        "v1",
        "revert must undo the commit in the tree"
    );
    assert_eq!(json_of(&r[tag_del].text)["effect"]["deleted"], true);

    assert_eq!(rig.git_out(&["tag", "--list"]), "", "tag must be deleted");
    // revert leaves the change uncommitted: HEAD is still `second`.
    assert_eq!(rig.git_out(&["log", "--format=%s", "-1"]), "second");
    assert_eq!(rig.read("a.txt"), "v1\n");
}

/// main and side change the same line of f.txt, so merging side into main conflicts.
fn conflicting_history(s: &mut Script) {
    s.shell(INIT);
    s.tool(
        "write_file",
        json!({"path":"f.txt","content":"line-base\n"}),
    );
    s.shell("git add f.txt; git commit -q -m base");
    s.shell("git checkout -q -b side");
    s.edit("f.txt", "line-base", "line-side");
    s.shell("git commit -q -am side");
    s.shell("git checkout -q main");
    s.edit("f.txt", "line-base", "line-main");
    s.shell("git commit -q -am main-change");
}

#[test]
fn git_merge_surfaces_conflicts_aborts_resolves_manually_and_completes() {
    let mut s = Script::default();
    conflicting_history(&mut s);
    let start = s.tool(
        "git_merge",
        json!({"operation":"merge_start","source":"side"}),
    );
    let conflicts = s.tool("git_merge", json!({"operation":"merge_conflicts"}));
    let refused = s.tool("git_merge", json!({"operation":"merge_continue"}));
    let abort = s.tool("git_merge", json!({"operation":"merge_abort"}));
    let after_abort = s.shell(&cat_cmd("f.txt"));
    let clean = s.tool("git_merge", json!({"operation":"merge_conflicts"}));
    let restart = s.tool(
        "git_merge",
        json!({"operation":"merge_start","source":"side"}),
    );
    let not_conflict = s.tool(
        "git_merge",
        json!({"operation":"resolve_manual","path":"README-none.md","content":"x"}),
    );
    let manual = s.tool(
        "git_merge",
        json!({"operation":"resolve_manual","path":"f.txt","content":"line-resolved\n"}),
    );
    let none_left = s.tool("git_merge", json!({"operation":"merge_conflicts"}));
    let cont = s.tool(
        "git_merge",
        json!({"operation":"merge_continue","message":"merge side"}),
    );
    let again = s.tool(
        "git_merge",
        json!({"operation":"merge_start","source":"side"}),
    );
    let bad_source = s.tool(
        "git_merge",
        json!({"operation":"merge_start","source":"no-such-branch"}),
    );
    let total = s.steps.len();

    let p = sequence(s.steps, "done");
    let rig = Rig::new_unversioned(&p, &[]);
    let out = rig.run_with_deadline("go", 300);
    let r = expect_git_results(&rig, &out, total);

    let started = json_of(&r[start].text);
    assert_eq!(started["effect"]["status"], "conflicts", "{started}");
    assert_eq!(started["effect"]["conflicts"][0]["path"], "f.txt");
    let listed = json_of(&r[conflicts].text);
    assert_eq!(listed["effect"]["hasConflicts"], true);
    assert_eq!(listed["effect"]["conflicts"][0]["path"], "f.txt");
    assert_eq!(listed["effect"]["conflicts"][0]["stages"]["ours"], true);
    assert_eq!(listed["effect"]["conflicts"][0]["stages"]["theirs"], true);
    assert_eq!(listed["effect"]["conflicts"][0]["stages"]["ancestor"], true);

    assert!(r[refused].error);
    assert_eq!(
        r[refused].text,
        "Refusing merge_continue: unresolved conflicts remain. Resolve every path first."
    );
    assert_eq!(json_of(&r[abort].text)["effect"]["aborted"], true);
    assert_eq!(
        r[after_abort].text.trim(),
        "line-main",
        "abort restores our side"
    );
    assert_eq!(json_of(&r[clean].text)["effect"]["hasConflicts"], false);

    assert_eq!(json_of(&r[restart].text)["effect"]["status"], "conflicts");
    assert!(r[not_conflict].error);
    assert_eq!(
        r[not_conflict].text,
        "'README-none.md' is not a listed conflict."
    );
    assert_eq!(json_of(&r[manual].text)["effect"]["resolvedWith"], "manual");
    assert_eq!(json_of(&r[none_left].text)["effect"]["hasConflicts"], false);
    let committed = json_of(&r[cont].text);
    let sha = committed["effect"]["commit"]
        .as_str()
        .expect("merge commit");
    assert_eq!(rig.git_out(&["rev-parse", "HEAD"]), sha);
    assert_eq!(rig.git_out(&["log", "--format=%s", "-1"]), "merge side");
    assert_eq!(
        rig.git_out(&["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count(),
        3,
        "a real two-parent merge commit"
    );
    assert_eq!(rig.git_out(&["show", "HEAD:f.txt"]), "line-resolved");
    assert_eq!(json_of(&r[again].text)["effect"]["status"], "up_to_date");
    assert!(r[bad_source].error);
    assert!(
        r[bad_source]
            .text
            .starts_with("Unknown merge source 'no-such-branch'"),
        "{}",
        r[bad_source].text
    );
}

#[test]
fn git_merge_rebase_conflicts_resolve_theirs_and_continue_to_a_linear_history() {
    let mut s = Script::default();
    conflicting_history(&mut s);
    let start = s.tool(
        "git_merge",
        json!({"operation":"rebase_start","source":"side"}),
    );
    let conflicts = s.tool("git_merge", json!({"operation":"rebase_conflicts"}));
    let refused = s.tool("git_merge", json!({"operation":"rebase_continue"}));
    let abort = s.tool("git_merge", json!({"operation":"rebase_abort"}));
    let restart = s.tool(
        "git_merge",
        json!({"operation":"rebase_start","source":"side"}),
    );
    let theirs = s.tool(
        "git_merge",
        json!({"operation":"resolve_theirs","path":"f.txt"}),
    );
    let cont = s.tool("git_merge", json!({"operation":"rebase_continue"}));
    let total = s.steps.len();

    let p = sequence(s.steps, "done");
    let rig = Rig::new_unversioned(&p, &[]);
    let out = rig.run_with_deadline("go", 300);
    let r = expect_git_results(&rig, &out, total);

    let started = json_of(&r[start].text);
    assert_eq!(started["effect"]["status"], "conflicts", "{started}");
    assert_eq!(
        json_of(&r[conflicts].text)["effect"]["conflicts"][0]["path"],
        "f.txt"
    );
    assert!(r[refused].error);
    assert_eq!(
        r[refused].text,
        "Refusing rebase_continue: unresolved conflicts remain. Resolve every path first."
    );
    assert_eq!(json_of(&r[abort].text)["effect"]["aborted"], true);
    assert_eq!(json_of(&r[restart].text)["effect"]["status"], "conflicts");
    assert_eq!(json_of(&r[theirs].text)["effect"]["resolvedWith"], "theirs");
    assert_eq!(json_of(&r[cont].text)["effect"]["status"], "completed");

    // main's own change was replayed on top of side, resolved in favour of the replayed commit.
    assert_eq!(
        rig.git_out(&["log", "--format=%s"]),
        "main-change\nside\nbase"
    );
    assert_eq!(rig.git_out(&["show", "HEAD:f.txt"]), "line-main");
    assert_eq!(rig.git_out(&["branch", "--show-current"]), "main");
}

// ───────────────────────────── remote-facing git tools ────────────────────────

fn git_cli(dir: &std::path::Path, args: &[&str]) -> String {
    let o = rightkit_child_process::command_std("git")
        .args(["-c", "user.name=e2e", "-c", "user.email=e2e@example.test"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn git_fetch_updates_remote_tracking_refs_only_and_guards_the_remote() {
    // A real upstream repository on disk (a local path remote: no network).
    let upstream = tempfile::TempDir::new().unwrap();
    git_cli(upstream.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(upstream.path().join("up.txt"), "from-upstream\n").unwrap();
    git_cli(upstream.path(), &["add", "up.txt"]);
    git_cli(upstream.path(), &["commit", "-q", "-m", "upstream-1"]);
    let upstream_sha = git_cli(upstream.path(), &["rev-parse", "HEAD"]);
    let upstream_path = upstream.path().to_string_lossy().replace('\\', "/");

    let mut s = Script::default();
    s.shell(INIT);
    s.tool("write_file", json!({"path":"local.txt","content":"mine\n"}));
    s.shell("git add local.txt; git commit -q -m local-1");
    s.shell(&format!("git remote add origin {upstream_path}"));
    s.tool(
        "write_file",
        json!({"path":"untracked.txt","content":"keep-me\n"}),
    );
    let head_before = s.shell("git rev-parse HEAD");
    let fetch = s.tool("git_fetch", json!({}));
    let branches = s.tool("git_read", json!({"operation":"branch"}));
    let guard_ok = s.tool(
        "git_fetch",
        json!({"expected_remote_url": upstream_path.clone()}),
    );
    let guard_bad = s.tool(
        "git_fetch",
        json!({"expected_remote_url":"https://elsewhere.invalid/x.git"}),
    );
    let unknown = s.tool("git_fetch", json!({"remote":"nope"}));
    let head_after = s.shell("git rev-parse HEAD");
    let total = s.steps.len();

    let p = sequence(s.steps, "done");
    let rig = Rig::new_unversioned(&p, &[]);
    let out = rig.run("go");
    let r = expect_git_results(&rig, &out, total);

    let fetched = json_of(&r[fetch].text);
    assert_eq!(fetched["operation"], "fetch");
    assert_eq!(fetched["effect"]["remote"], "origin");
    assert!(
        fetched["effect"]["receivedObjects"].as_u64().unwrap_or(0) >= 1,
        "{fetched}"
    );
    let rows = json_of(&r[branches].text);
    let tracking = rows["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "origin/main")
        .unwrap_or_else(|| panic!("origin/main must exist after fetch: {rows}"));
    assert_eq!(tracking["kind"], "remote");
    assert_eq!(tracking["target"], upstream_sha.as_str());
    assert_eq!(rig.git_out(&["rev-parse", "origin/main"]), upstream_sha);

    assert!(!r[guard_ok].error, "{}", r[guard_ok].text);
    assert!(r[guard_bad].error);
    assert!(
        r[guard_bad]
            .text
            .starts_with("Remote URL changed: expected 'https://elsewhere.invalid/x.git'"),
        "{}",
        r[guard_bad].text
    );
    assert!(r[unknown].error);
    assert!(
        r[unknown].text.starts_with("Unknown remote 'nope'"),
        "{}",
        r[unknown].text
    );

    // Fetch never touches HEAD, index or the working tree.
    assert_eq!(r[head_before].text.trim(), r[head_after].text.trim());
    assert_eq!(rig.read("untracked.txt"), "keep-me\n");
    assert!(!rig.exists("up.txt"), "fetch must not check files out");
}

#[test]
fn git_clone_enforces_its_guards_and_cleans_up_a_failed_clone() {
    let mut s = Script::default();
    s.shell(INIT);
    let plain_http = s.tool(
        "git_clone",
        json!({"remote_url":"http://127.0.0.1:1/x.git","repo_path":"c1"}),
    );
    let creds = s.tool(
        "git_clone",
        json!({"remote_url":"https://user:pw@127.0.0.1:1/x.git","repo_path":"c2"}),
    );
    s.tool(
        "write_file",
        json!({"path":"occupied/keep.txt","content":"k\n"}),
    );
    let occupied = s.tool(
        "git_clone",
        json!({"remote_url":"https://127.0.0.1:1/x.git","repo_path":"occupied"}),
    );
    let escape = s.tool(
        "git_clone",
        json!({"remote_url":"https://127.0.0.1:1/x.git","repo_path":"../outside"}),
    );
    let unreachable = s.tool(
        "git_clone",
        json!({"remote_url":"https://127.0.0.1:1/x.git","repo_path":"fresh"}),
    );
    let total = s.steps.len();

    let p = sequence(s.steps, "done");
    let rig = Rig::new_unversioned(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, total);

    assert!(r[plain_http].error);
    assert_eq!(r[plain_http].text, "git_clone accepts HTTPS remotes only.");
    assert!(r[creds].error);
    assert_eq!(r[creds].text, "Remote URL must not contain credentials.");
    assert!(
        !rig.exists("c1") && !rig.exists("c2"),
        "refused clones create nothing"
    );
    assert!(r[occupied].error);
    assert!(
        r[occupied].text.contains("is not empty."),
        "{}",
        r[occupied].text
    );
    assert_eq!(rig.read("occupied/keep.txt"), "k\n");
    assert!(r[escape].error, "{}", r[escape].text);
    assert!(r[unreachable].error);
    assert!(
        r[unreachable].text.starts_with("Clone failed:"),
        "{}",
        r[unreachable].text
    );
    assert!(
        !rig.exists("fresh"),
        "a failed clone must remove its partial destination"
    );
}

const POINTER_A: &str = "version https://git-lfs.github.com/spec/v1\noid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsize 1234\n";
const POINTER_B: &str = "version https://git-lfs.github.com/spec/v1\noid sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\nsize 66\n";

#[test]
fn git_lfs_scan_lists_pointers_and_lfs_fetch_enforces_its_bounds_before_any_network() {
    let upstream = tempfile::TempDir::new().unwrap();
    let upstream_path = upstream.path().to_string_lossy().replace('\\', "/");
    let mut s = Script::default();
    s.shell(INIT);
    s.tool(
        "write_file",
        json!({"path":"assets/model.bin","content":POINTER_A}),
    );
    s.tool("write_file", json!({"path":"data.bin","content":POINTER_B}));
    s.tool(
        "write_file",
        json!({"path":"not-a-pointer.txt","content":"just text\n"}),
    );
    s.shell("git add .; git commit -q -m pointers");
    let scan = s.tool("git_lfs_scan", json!({}));
    let too_big = s.tool(
        "git_lfs_fetch",
        json!({"paths":["assets/model.bin"],"max_bytes":10}),
    );
    let wildcard = s.tool(
        "git_lfs_fetch",
        json!({"paths":["assets/*.bin"],"max_bytes":5000}),
    );
    let not_pointer = s.tool(
        "git_lfs_fetch",
        json!({"paths":["not-a-pointer.txt"],"max_bytes":5000}),
    );
    let no_remote = s.tool(
        "git_lfs_fetch",
        json!({"paths":["assets/model.bin"],"max_bytes":5000}),
    );
    s.shell(&format!("git remote add origin {upstream_path}"));
    let not_https = s.tool(
        "git_lfs_fetch",
        json!({"paths":["assets/model.bin"],"max_bytes":5000}),
    );
    let none = s.tool("git_lfs_fetch", json!({"paths":[],"max_bytes":5000}));
    let total = s.steps.len();

    let p = sequence(s.steps, "done");
    let rig = Rig::new_unversioned(&p, &[]);
    let out = rig.run("go");
    let r = expect_git_results(&rig, &out, total);

    let scanned = json_of(&r[scan].text);
    assert_eq!(scanned["operation"], "lfs_scan");
    let mut pointers: Vec<(String, String, u64)> = scanned["pointers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["path"].as_str().unwrap().to_string(),
                p["oid"].as_str().unwrap().to_string(),
                p["size"].as_u64().unwrap(),
            )
        })
        .collect();
    pointers.sort();
    assert_eq!(
        pointers,
        vec![
            ("assets/model.bin".to_string(), "a".repeat(64), 1234),
            ("data.bin".to_string(), "b".repeat(64), 66),
        ]
    );
    assert_eq!(scanned["totalDeclaredBytes"], 1300);

    assert!(r[too_big].error);
    assert!(
        r[too_big]
            .text
            .starts_with("Refusing LFS fetch: declared size 1234 bytes exceeds max_bytes 10."),
        "{}",
        r[too_big].text
    );
    assert!(r[wildcard].error);
    assert_eq!(
        r[wildcard].text,
        "Invalid exact LFS pointer path 'assets/*.bin'."
    );
    assert!(r[not_pointer].error);
    assert_eq!(
        r[not_pointer].text,
        "'not-a-pointer.txt' is not a tracked LFS pointer file."
    );
    assert!(r[no_remote].error);
    assert!(
        r[no_remote].text.starts_with("Unknown remote 'origin'"),
        "{}",
        r[no_remote].text
    );
    assert!(r[not_https].error);
    assert!(
        r[not_https]
            .text
            .contains("LFS batch transfer requires an HTTPS remote"),
        "{}",
        r[not_https].text
    );
    assert!(r[none].error);
    assert_eq!(
        r[none].text,
        "git_lfs_fetch requires at least one path; whole-repo LFS fetch is refused."
    );
    assert!(
        !rig.root().join(".git").join("lfs").join("objects").exists(),
        "a refused fetch must not create LFS object storage"
    );
}
