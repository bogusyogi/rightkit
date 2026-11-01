//! End-to-end Blueprint journeys: a real temp git repository with Rust, TypeScript and
//! Python sources is built, queried, edited, refreshed, corrupted and re-read through the
//! public one-shot entry points (`rightkit_repo_graph::cli` / `native_blueprint_operation`).
//! Nothing here inspects private modules; persisted facts are read back from the SQLite
//! store the same way an independent consumer would.

use rightkit_repo_graph::cli;
use rightkit_repo_graph::{
    native_blueprint_operation, BlueprintApi, BlueprintRequest, Bounds, CancellationToken,
    Operation,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tempfile::{tempdir, TempDir};

const RUST_LIB: &str = "pub fn compute_total(items: &[u32]) -> u32 {\n    items.iter().sum()\n}\n\npub fn report(items: &[u32]) -> u32 {\n    compute_total(items)\n}\n";
const TS_UTIL: &str =
    "export function formatPrice(n: number): string {\n  return '$' + n.toFixed(2);\n}\n";
const TS_APP: &str = "import { formatPrice } from './util';\nexport function renderCart(n: number): string {\n  return formatPrice(n);\n}\n";
const PY_HELPERS: &str = "def normalize_name(value):\n    return value.strip()\n";
const PY_MAIN: &str = "from .helpers import normalize_name\n\ndef clean_input(value):\n    return normalize_name(value)\n\ndef handle_request(value):\n    return clean_input(value)\n";

/// Serializes the journeys of this file. Each one builds its own repository, but the freshness
/// probe runs bounded git calls and index builds that compete for the CPU when the tests run in
/// parallel, so a sibling journey can miss a probe budget and read "unavailable" (#441). The
/// budgets themselves are unchanged.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn git(root: &Path, args: &[&str]) {
    let out = crate::hidden_command("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// Multi-language repository committed to git; `.agent/` (Blueprint's own store) is ignored so
/// worktree freshness reflects only user source. It deliberately has no README.md: a build
/// appends a link block to an existing README (known bug, see the `bug_` test), which would make
/// every post-build freshness read stale for reasons unrelated to what each journey asserts.
fn fixture() -> TempDir {
    let root = tempdir().unwrap();
    let r = root.path();
    git(r, &["init", "--quiet"]);
    git(r, &["config", "user.email", "e2e@example.invalid"]);
    git(r, &["config", "user.name", "E2E"]);
    write(r, ".gitignore", ".agent/\nnode_modules/\nignored_dir/\n");
    write(r, "src/lib.rs", RUST_LIB);
    write(r, "web/util.ts", TS_UTIL);
    write(r, "web/app.ts", TS_APP);
    write(r, "svc/helpers.py", PY_HELPERS);
    write(r, "svc/main.py", PY_MAIN);
    git(r, &["add", "-A"]);
    git(r, &["commit", "--quiet", "-m", "seed"]);
    root
}

fn root_str(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

fn build(root: &Path) -> Value {
    let response = cli::run_one_shot("e2e-build", Operation::Build, root_str(root), Some(120_000));
    assert!(response.ok, "build failed: {:?}", response.error);
    response.result.expect("build result")
}

fn refresh(root: &Path) -> Value {
    cli::manual_refresh(root_str(root), Some(30_000)).expect("refresh")
}

fn status(root: &Path) -> Value {
    // Status under a loaded machine: the live freshness probe (git, file stats) can miss its slice
    // when the gate runs other tests in parallel and answer "unavailable" (#441). That is load, not
    // the behaviour under test, so a probe that could not answer is asked again, a few times.
    let mut last = Value::Null;
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        last = cli::status(root_str(root), Some(30_000)).expect("status");
        if last["state"] != "unavailable" {
            break;
        }
    }
    last
}

fn graph(root: &Path) -> rightkit_repo_graph::store::Generation {
    let db = root.join(".agent/graph/graph.db");
    let connection = rightkit_repo_graph::store::open_store_read_only(&db).expect("open store");
    rightkit_repo_graph::store::load_generation(&connection)
        .expect("load")
        .expect("generation")
}

fn symbol_names(root: &Path) -> Vec<String> {
    graph(root)
        .nodes
        .iter()
        .filter(|n| n["kind"] != "file")
        .filter_map(|n| n["name"].as_str().map(str::to_owned))
        .collect()
}

fn has_symbol(root: &Path, name: &str) -> bool {
    symbol_names(root).iter().any(|n| n == name)
}

fn search_hits(root: &Path, query: &str) -> Vec<(String, String)> {
    let result = cli::search(root_str(root), query, Some(50), Some(30_000)).expect("search");
    result["candidates"]
        .as_array()
        .map(|c| {
            c.iter()
                .map(|n| {
                    (
                        n["name"].as_str().unwrap_or_default().to_owned(),
                        n["path"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Content digest of every user-visible file, excluding Blueprint's own store and git metadata.
fn worktree_digest(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == ".git" || rel == ".agent" {
                continue;
            }
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn multi_language_build_indexes_symbols_and_resolves_references() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    let built = build(r);
    assert_eq!(built["state"], "fresh");
    assert!(built["counts"]["files"].as_u64().unwrap() >= 5);

    // Symbols from every language are indexed.
    for symbol in [
        "compute_total",
        "report",
        "formatPrice",
        "renderCart",
        "normalize_name",
        "handle_request",
    ] {
        assert!(has_symbol(r, symbol), "missing symbol {symbol}");
    }

    // Search finds the symbol in the file that defines it.
    let hits = search_hits(r, "formatPrice");
    assert!(
        hits.iter()
            .any(|(name, path)| name == "formatPrice" && path == "web/util.ts"),
        "{hits:?}"
    );
    let hits = search_hits(r, "compute_total");
    assert!(
        hits.iter().any(|(_, path)| path == "src/lib.rs"),
        "{hits:?}"
    );

    // Cross-file import edges resolve for TypeScript and Python; same-file calls resolve.
    let g = graph(r);
    let resolved_import = |from: &str, to: &str| {
        g.edges.iter().any(|e| {
            e["kind"] == "IMPORTS"
                && e["source"] == format!("file:{from}")
                && e["target"] == format!("file:{to}")
                && e["resolved"] == true
        })
    };
    assert!(
        resolved_import("web/app.ts", "web/util.ts"),
        "TS import unresolved"
    );
    assert!(
        resolved_import("svc/main.py", "svc/helpers.py"),
        "Python import unresolved"
    );
    assert!(
        g.edges.iter().any(|e| e["kind"] == "CALLS"
            && e["target"]
                .as_str()
                .is_some_and(|t| t.contains("compute_total"))),
        "Rust call edge missing"
    );
    assert!(
        g.edges.iter().any(|e| e["kind"] == "CALLS"
            && e["target"]
                .as_str()
                .is_some_and(|t| t.contains("clean_input"))),
        "Python call edge missing"
    );
}

#[test]
fn expand_and_impact_follow_call_chains() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    build(r);

    let expanded =
        cli::expand(root_str(r), "handle_request", None, Some(3), Some(30_000)).expect("expand");
    let text = expanded["nodes"].to_string();
    assert!(
        text.contains("clean_input"),
        "expand did not reach callee: {text}"
    );

    let recalled = cli::recall(root_str(r), "handle_request", None, Some(30_000)).expect("recall");
    assert!(recalled.to_string().contains("handle_request"));

    // Impact of changing the leaf includes its transitive callers.
    let impact = cli::run_query(
        "e2e-impact",
        Operation::Impact,
        root_str(r),
        json!({"seed": "clean_input", "direction": "incoming"}),
        Some(30_000),
    )
    .expect("impact");
    assert!(
        impact["nodes"].to_string().contains("handle_request"),
        "impact: {impact}"
    );
}

#[test]
fn edits_are_reported_stale_then_incremental_refresh_converges() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    let first = build(r);
    let first_generation = first["generationId"].as_str().unwrap().to_owned();
    assert_eq!(status(r)["state"], "fresh");

    // Rename a function, add a file, delete a file; nothing refreshed yet.
    write(
        r,
        "src/lib.rs",
        &RUST_LIB.replace("compute_total", "compute_sum"),
    );
    write(
        r,
        "svc/extra.py",
        "def brand_new_feature():\n    return 1\n",
    );
    fs::remove_file(r.join("web/util.ts")).unwrap();

    let stale = status(r);
    assert_eq!(stale["state"], "stale", "{stale}");
    let stale_paths = stale["freshnessReceipt"]["staleSources"]["paths"].to_string();
    assert!(stale_paths.contains("src/lib.rs"), "{stale_paths}");

    // Reads never mutate: the old symbols are still what's persisted.
    assert!(has_symbol(r, "compute_total"));
    assert!(!has_symbol(r, "brand_new_feature"));

    let refreshed = refresh(r);
    assert_eq!(refreshed["refreshMode"], "incremental", "{refreshed}");
    assert_ne!(
        refreshed["generationId"].as_str().unwrap(),
        first_generation
    );

    assert!(has_symbol(r, "compute_sum"));
    assert!(
        !has_symbol(r, "compute_total"),
        "renamed symbol must be gone"
    );
    assert!(has_symbol(r, "brand_new_feature"));
    assert!(
        !has_symbol(r, "formatPrice"),
        "deleted file's symbols must be gone"
    );
    assert!(search_hits(r, "formatPrice")
        .iter()
        .all(|(name, _)| name != "formatPrice"));
    assert!(search_hits(r, "brand_new_feature")
        .iter()
        .any(|(_, path)| path == "svc/extra.py"));

    // The importer of the deleted file no longer has a resolved edge to it.
    let g = graph(r);
    assert!(!g
        .edges
        .iter()
        .any(|e| e["target"] == "file:web/util.ts" && e["resolved"] == true));

    // Idempotent: a second build over an unchanged tree is a no-op.
    let again = build(r);
    assert_eq!(again["refreshMode"], "incremental_noop");
    assert_eq!(status(r)["state"], "fresh");
}

#[test]
fn persisted_generation_survives_restart_and_fails_closed_on_mismatch() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    let built = build(r);
    let generation = built["generationId"].as_str().unwrap().to_owned();

    // A brand-new operation owner ("process restart") serves the persisted generation.
    let fresh_owner = native_blueprint_operation();
    let mut request = BlueprintRequest::new("restart-search", Operation::Search, root_str(r));
    request.deadline_ms = 30_000;
    request.generation = Some(generation.clone());
    request.input["query"] = Value::String("renderCart".into());
    let response = fresh_owner.dispatch(request, CancellationToken::new());
    assert!(response.ok, "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["generationId"], generation);
    assert!(result["candidates"].to_string().contains("web/app.ts"));

    // A caller pinned to a generation that is not the served one is refused.
    let mut pinned = BlueprintRequest::new("restart-pin", Operation::Search, root_str(r));
    pinned.deadline_ms = 30_000;
    pinned.generation = Some("generation-from-another-universe".into());
    pinned.input["query"] = Value::String("renderCart".into());
    let refused = fresh_owner.dispatch(pinned, CancellationToken::new());
    assert!(!refused.ok);
    assert!(refused.error.unwrap().code.contains("generation_mismatch"));
}

#[test]
fn corrupt_store_is_reported_and_rebuild_recovers() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    build(r);
    let db = r.join(".agent/graph/graph.db");
    fs::write(&db, b"this is not a sqlite database at all").unwrap();

    // Diagnostics must classify, not panic.
    let doctor = cli::doctor(root_str(r), false);
    assert!(
        doctor.to_string().to_lowercase().contains("corrupt"),
        "{doctor}"
    );

    // Rebuilding recovers a fully usable graph.
    let rebuilt = cli::run_one_shot("e2e-rebuild", Operation::Build, root_str(r), Some(120_000));
    assert!(
        rebuilt.ok,
        "rebuild after corruption failed: {:?}",
        rebuilt.error
    );
    assert!(has_symbol(r, "handle_request"));
    assert_eq!(status(r)["state"], "fresh");
}

#[test]
fn ignored_and_vendored_paths_are_not_indexed() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    write(
        r,
        "node_modules/dep/index.js",
        "function vendoredSecretSymbol() {}\n",
    );
    write(
        r,
        "ignored_dir/skip.py",
        "def ignored_dir_symbol():\n    pass\n",
    );
    build(r);
    assert!(!has_symbol(r, "vendoredSecretSymbol"));
    assert!(!has_symbol(r, "ignored_dir_symbol"));
    let files: Vec<String> = graph(r)
        .nodes
        .iter()
        .filter(|n| n["kind"] == "file")
        .filter_map(|n| n["path"].as_str().map(str::to_owned))
        .collect();
    assert!(
        files
            .iter()
            .all(|p| !p.starts_with("node_modules/") && !p.starts_with("ignored_dir/")),
        "{files:?}"
    );
    assert!(files.iter().any(|p| p == "src/lib.rs"));
}

#[test]
fn snapshot_then_edit_reports_changed_files() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    build(r);
    // Snapshots are only taken of a clean, committed worktree (lib_application_snapshots.rs:336 refuses dirty).
    git(r, &["add", "-A"]);
    git(r, &["commit", "--quiet", "--allow-empty", "-m", "baseline"]);
    refresh(r);
    cli::snapshot_create(root_str(r), "before-edit").expect("snapshot");
    let listed = cli::snapshot_list(root_str(r), Some(30_000)).expect("list");
    assert!(listed.to_string().contains("before-edit"));

    write(
        r,
        "svc/helpers.py",
        "def normalize_name(value):\n    return value.strip().lower()\n",
    );
    write(r, "svc/added.py", "def added_later():\n    return 2\n");
    refresh(r);

    let changed = cli::changes(
        root_str(r),
        Some("before-edit"),
        None,
        None,
        Some(50),
        Some(30_000),
    )
    .expect("changes");
    let text = changed.to_string();
    assert!(text.contains("svc/helpers.py"), "{text}");
    assert!(text.contains("svc/added.py"), "{text}");
    assert!(
        !text.contains("web/app.ts"),
        "unchanged file must not be reported: {text}"
    );
}

#[test]
fn findings_export_sarif_after_build() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    build(r);
    let sarif = cli::run_query(
        "e2e-sarif",
        Operation::FindingsSarif,
        root_str(r),
        json!({}),
        Some(30_000),
    )
    .expect("sarif");
    assert_eq!(sarif["kind"], "findings.sarif");
    let findings = cli::run_query(
        "e2e-findings",
        Operation::FindingsGet,
        root_str(r),
        json!({}),
        Some(30_000),
    )
    .expect("findings");
    assert_eq!(findings["kind"], "findings.get");
}

#[test]
fn architecture_orientation_names_the_relevant_files_for_a_task() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    build(r);
    let orientation = cli::architecture_orientation(root_str(r), "renderCart", Some(30_000))
        .expect("architecture");
    assert!(
        orientation.to_string().contains("web/app.ts"),
        "{orientation}"
    );
}

#[test]
fn invalid_roots_and_cancelled_requests_are_typed_failures() {
    let _serial = serial();
    // Relative / missing roots are refused rather than silently indexing the cwd.
    let missing = cli::run_one_shot(
        "e2e-missing",
        Operation::Build,
        "definitely/not/a/real/root",
        Some(30_000),
    );
    assert!(!missing.ok);
    let gone = tempdir().unwrap().path().join("deleted");
    assert!(!cli::run_one_shot("e2e-gone", Operation::Build, root_str(&gone), Some(30_000)).ok);

    // A pre-cancelled build publishes nothing.
    let repo = fixture();
    let r = repo.path();
    let op = native_blueprint_operation();
    let mut request = BlueprintRequest::new("cancelled", Operation::Build, root_str(r));
    request.deadline_ms = 120_000;
    let mut context = request.validate(Bounds::one_shot()).unwrap();
    let token = CancellationToken::new();
    token.cancel();
    context.cancellation = token;
    let outcome = op.execute(&request, &context);
    assert!(outcome.is_err(), "cancelled build must not succeed");
    assert!(!r.join(".agent/graph/graph.db").exists() || graph_is_absent_or_unpublished(r));
}

fn graph_is_absent_or_unpublished(root: &Path) -> bool {
    let db = root.join(".agent/graph/graph.db");
    match rightkit_repo_graph::store::open_store_read_only(&db) {
        Ok(c) => matches!(
            rightkit_repo_graph::store::load_generation(&c),
            Ok(None) | Err(_)
        ),
        Err(_) => true,
    }
}

/// BUG: Blueprint writes `docs/` and a README block into the user's repository on every
/// build (`engine.rs` build path -> `lib_generated_docs::generate_docs`); only `noReadmeLink`
/// opts out of the link. A read-only intelligence build must leave user files untouched.
#[test]
fn bug_build_leaves_user_working_tree_unchanged() {
    let _serial = serial();
    let repo = fixture();
    let r = repo.path();
    write(r, "README.md", "# Demo\n\nA tiny multi-language fixture.\n");
    git(r, &["add", "-A"]);
    git(r, &["commit", "--quiet", "-m", "readme"]);
    let before = worktree_digest(r);
    build(r);
    refresh(r);
    let after = worktree_digest(r);
    let porcelain = crate::hidden_command("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(r)
        .output()
        .unwrap();
    let porcelain = String::from_utf8_lossy(&porcelain.stdout).into_owned();
    assert!(
        porcelain.trim().is_empty(),
        "git sees changes after a read-only build:\n{porcelain}"
    );
    let added: Vec<_> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    let modified: Vec<_> = before
        .iter()
        .filter(|(k, v)| after.get(*k) != Some(*v))
        .map(|(k, _)| k)
        .collect();
    assert!(
        added.is_empty() && modified.is_empty(),
        "build changed the repo: added={added:?} modified={modified:?}"
    );
}

/// Spawn a test child without a visible Windows console (CREATE_NO_WINDOW).
#[allow(clippy::disallowed_methods)]
fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    command
}
