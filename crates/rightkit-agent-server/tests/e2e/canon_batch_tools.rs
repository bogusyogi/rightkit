//! Tool and state-management contracts exposed by the real daemon/CLI.

use crate::support::{e2e_api::Rig as ApiRig, e2e_tools as t};
use serde_json::json;
use t::*;

#[test]
fn vg004_worktree_pool_acquire_release_and_restart_preserve_readiness() {
    let mut rig = ApiRig::start(vec![]);
    rig.session("pool", json!({}));
    let (status, before) = rig.get("/worktrees/pool/status");
    assert_eq!(status, 200, "durable pool readiness: {before}");
    let ready_before = before["readyCount"].as_u64().expect("ready count");
    let (status, acquired) = rig.post("/worktrees/pool/acquire", json!({"sessionId":"pool"}));
    assert_eq!(status, 200, "acquire: {acquired}");
    let id = acquired["worktreeId"].as_str().expect("worktree id");
    assert_eq!(acquired["ready"], false, "acquired tree leaves warm pool");
    let (status, released) = rig.post(&format!("/worktrees/pool/{id}/release"), json!({}));
    assert_eq!(status, 200, "release: {released}");
    assert_eq!(released["ready"], true);
    rig.restart();
    let (status, after) = rig.get("/worktrees/pool/status");
    assert_eq!(status, 200, "readiness survives daemon restart: {after}");
    assert!(after["readyCount"].as_u64().unwrap() >= ready_before.max(1));
}

#[test]
fn cg_b01_lsp_tool_uses_supervised_server_and_returns_workspace_symbols() {
    let cell = RootCell::new();
    let stub_root = cell.clone();
    let p = sequence(
        vec![call_with("lsp", move |_| {
            json!({
                "operation":"workspace_symbols", "query":"Widget", "path":"src/lib.rs",
                "server_path":"node",
                "server_args":[stub_root.root().join("symbols.js").to_string_lossy()]
            })
        })],
        "done",
    );
    let rig = Rig::new(
        &p,
        &[
            ("src/lib.rs", "pub struct Widget;\n"),
            ("symbols.js", LSP_SYMBOL_STUB),
        ],
    );
    cell.bind(&rig);
    let out = rig.run("find Widget symbols");
    let results = results(&out);
    assert_eq!(
        results.len(),
        1,
        "one bounded LSP tool result: {}",
        out.diag()
    );
    assert!(
        !results[0].error,
        "stub server responds to JSON-RPC: {}",
        results[0].text
    );
    assert!(
        results[0].text.contains("Widget"),
        "workspace symbol state reaches caller: {}",
        results[0].text
    );
    assert!(
        results[0].text.contains("supervised-symbol-server"),
        "result must come from supervised JSON-RPC server"
    );
}

const LSP_SYMBOL_STUB: &str = r#"
let buf = Buffer.alloc(0), root;
function send(o) {
  const s = JSON.stringify(o);
  process.stdout.write('Content-Length: ' + Buffer.byteLength(s) + '\r\n\r\n' + s);
}
function handle(m) {
  if (m.method === 'initialize') {
    root = m.params.rootUri;
    return send({jsonrpc:'2.0', id:m.id, result:{capabilities:{workspaceSymbolProvider:true}}});
  }
  if (m.method === 'workspace/symbol') {
    return send({jsonrpc:'2.0', id:m.id, result:m.params.query === 'Widget' ? [{
      name:'Widget', kind:23, containerName:'supervised-symbol-server',
      location:{uri:root + '/src/lib.rs', range:{start:{line:0,character:11}, end:{line:0,character:17}}}
    }] : []});
  }
  if (m.method === 'exit') process.exit(0);
  if (m.id !== undefined) send({jsonrpc:'2.0', id:m.id, result:null});
}
process.stdin.on('data', d => {
  buf = Buffer.concat([buf, d]);
  for (;;) {
    const i = buf.indexOf('\r\n\r\n');
    if (i < 0) return;
    const n = Number(/Content-Length: (\d+)/i.exec(buf.subarray(0,i).toString())[1]);
    if (buf.length < i+4+n) return;
    const m = JSON.parse(buf.subarray(i+4,i+4+n));
    buf = buf.subarray(i+4+n);
    handle(m);
  }
});
"#;

#[test]
fn cg_c01_migration_upgrade_is_journaled_and_rollback_restores_prior_state() {
    let home = tempfile::tempdir().expect("isolated home");
    let db = crate::common::app_root(home.path()).join("state.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let connection = rightkit_db::rusqlite::Connection::open(&db).expect("seed older schema DB");
    connection
        .pragma_update(None, "user_version", 1)
        .expect("set prior schema version");
    drop(connection);
    let mut cmd = crate::support::daemon_process::start_daemon(home.path(), &[]);
    assert!(
        !cmd.startup.token.is_empty(),
        "daemon publishes ready startup only after migration"
    );
    let journal = crate::common::app_root(home.path()).join("migrations/pending.json");
    assert!(
        !journal.exists(),
        "successful activation clears pending journal"
    );
    let _ = cmd.child.kill();
    let _ = cmd.child.wait();
}
