//! Search / list_files journeys through the REAL `coderight run` binary: output modes, bounds,
//! ignore rules (gitignore, binary files, `.coderight` noise), globs and path-boundary errors.
//! The `lsp` tool is driven against a scripted stdio language server (a node stub speaking real
//! Content-Length JSON-RPC), so definition / references / hover / diagnostics and the failure
//! fallback are proven end to end without any installed language server.
//! Retires the unit tests of `search`, `list_files` and the ignore/bounds helpers.

use crate::support::e2e_tools as t;
use t::*;

fn norm(s: &str) -> String {
    s.replace('\\', "/")
}

/// Result lines without the trailing `[search status=...]` footer, normalised.
fn body_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !is_search_footer(l))
        .map(norm)
        .collect()
}

fn footer(text: &str) -> &str {
    text.lines().last().unwrap_or_default()
}

const FILES: &[(&str, &str)] = &[
    (".gitignore", "ignored/\n*.log\n"),
    ("src/a.rs", "fn needle() {}\n// needle again\n"),
    ("src/b.rs", "NEEDLE upper\n"),
    ("ignored/c.rs", "needle hidden\n"),
    ("build.log", "needle log\n"),
    ("docs/readme.md", "no match here\n"),
];

#[test]
fn search_modes_bounds_and_errors() {
    let p = sequence(
        vec![
            call("search", json!({"pattern":"needle"})),
            call("search", json!({"pattern":"needle","max_results":1})),
            call("search", json!({"pattern":"needle","output_mode":"count"})),
            call(
                "search",
                json!({"pattern":"needle","output_mode":"files_with_matches","case_sensitive":true}),
            ),
            call("search", json!({"pattern":"needle","file_glob":"b.rs"})),
            call("search", json!({"pattern":"again","context":1})),
            call("search", json!({"pattern":"zzzz-no-such-text"})),
            call("search", json!({"pattern":"(unclosed"})),
            call("search", json!({"pattern":"needle","path":"../"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, FILES);
    let out = rig.run("go");
    let r = expect_results(&out, 9);

    // 0: every non-ignored hit, sorted by path, case-insensitive by default.
    let lines = body_lines(&r[0].text);
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(
        lines[0].ends_with("src/a.rs:1: fn needle() {}"),
        "{lines:#?}"
    );
    assert!(
        lines[1].ends_with("src/a.rs:2: // needle again"),
        "{lines:#?}"
    );
    assert!(lines[2].ends_with("src/b.rs:1: NEEDLE upper"), "{lines:#?}");
    assert!(
        footer(&r[0].text).starts_with("[search status=Complete reason=none files="),
        "{}",
        footer(&r[0].text)
    );
    assert!(
        footer(&r[0].text).contains(" cursor=null]"),
        "{}",
        footer(&r[0].text)
    );

    // 1: result bound -> truncated with a continuation cursor.
    assert_eq!(body_lines(&r[1].text).len(), 1);
    assert!(
        footer(&r[1].text).starts_with("[search status=Truncated reason=result_ceiling files="),
        "{}",
        footer(&r[1].text)
    );
    assert!(
        !footer(&r[1].text).contains(" cursor=null]"),
        "{}",
        footer(&r[1].text)
    );

    // 2: per-file match-line counts.
    let counts = body_lines(&r[2].text);
    assert_eq!(counts.len(), 2, "{counts:#?}");
    assert!(counts[0].ends_with("src/a.rs:2"), "{counts:#?}");
    assert!(counts[1].ends_with("src/b.rs:1"), "{counts:#?}");

    // 3: case-sensitive + files_with_matches -> only the lowercase file.
    let files = body_lines(&r[3].text);
    assert_eq!(files.len(), 1, "{files:#?}");
    assert!(files[0].ends_with("src/a.rs"), "{files:#?}");

    // 4: file_glob restricts to one file name.
    let globbed = body_lines(&r[4].text);
    assert_eq!(globbed.len(), 1, "{globbed:#?}");
    assert!(
        globbed[0].ends_with("src/b.rs:1: NEEDLE upper"),
        "{globbed:#?}"
    );

    // 5: context lines use `-` after the line number, matches use `:`.
    let ctx = body_lines(&r[5].text);
    assert_eq!(ctx.len(), 2, "{ctx:#?}");
    assert!(ctx[0].ends_with("src/a.rs:1- fn needle() {}"), "{ctx:#?}");
    assert!(ctx[1].ends_with("src/a.rs:2: // needle again"), "{ctx:#?}");

    // 6: no match.
    assert!(
        r[6].text
            .starts_with("No matches found.\n[search status=Complete"),
        "{}",
        r[6].text
    );

    // 7-8: typed errors.
    assert!(r[7].error);
    assert!(r[7].text.starts_with("Invalid regex: "), "{}", r[7].text);
    assert!(r[8].error);
    assert!(
        r[8].text
            .starts_with("Path '../' escapes the workspace root '"),
        "{}",
        r[8].text
    );
}

#[test]
fn search_skips_ignored_binary_and_internal_files() {
    let p = sequence(
        vec![call(
            "search",
            json!({"pattern":"needle","output_mode":"files_with_matches"}),
        )],
        "done",
    );
    let rig = Rig::new(&p, FILES);
    // A binary file that contains the pattern; a NUL byte makes search skip it.
    rig.seed_bytes("blob.bin", b"needle\0\x01\x02 binary");
    let out = rig.run("go");
    let r = expect_results(&out, 1);

    let hits = body_lines(&r[0].text);
    assert_eq!(
        hits.len(),
        2,
        "only src/a.rs and src/b.rs may match: {hits:#?}"
    );
    assert!(hits.iter().all(|h| h.contains("/src/")), "{hits:#?}");
    // (the session root itself lives under `.coderight/worktrees`, so only check relative names)
    for hidden in [
        "ignored/c.rs",
        "build.log",
        "blob.bin",
        "checkpoints",
        "history",
    ] {
        assert!(
            !r[0].text.contains(hidden),
            "{hidden} must not be searched: {}",
            r[0].text
        );
    }
}

#[test]
fn list_files_respects_globs_bounds_and_ignore_rules() {
    let p = sequence(
        vec![
            call("list_files", json!({})),
            call("list_files", json!({"glob":"*.rs"})),
            call("list_files", json!({"glob":"src/**/*.rs"})),
            call("list_files", json!({"max":2})),
            call("list_files", json!({"path":"docs"})),
            call("list_files", json!({"path":"docs","glob":"*.rs"})),
            call("list_files", json!({"path":"../"})),
            call("list_files", json!({"glob":"[unclosed"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, FILES);
    let out = rig.run("go");
    let r = expect_results(&out, 8);

    // 0: sorted, relative, gitignore-aware, hidden files included, `.coderight` excluded.
    let all = norm(&r[0].text);
    let all: Vec<&str> = all.lines().collect();
    assert_eq!(
        all,
        vec![
            ".gitignore",
            "README.md",
            "docs/readme.md",
            "src/a.rs",
            "src/b.rs"
        ],
        "{}",
        r[0].text
    );

    // 1-2: bare glob matches file names, path glob matches the whole relative path.
    assert_eq!(norm(&r[1].text), "src/a.rs\nsrc/b.rs");
    assert_eq!(norm(&r[2].text), "src/a.rs\nsrc/b.rs");

    // 3: bound -> exactly two paths plus the truncation marker.
    let bounded: Vec<&str> = r[3].text.lines().collect();
    assert_eq!(bounded.len(), 3, "{}", r[3].text);
    assert_eq!(bounded[2], "[truncated at 2 files]");

    // 4-5: subdirectory listing and an empty result.
    assert_eq!(norm(&r[4].text), "docs/readme.md");
    assert_eq!(r[5].text, "No files found.");

    // 6-7: boundary and glob errors.
    assert!(r[6].error);
    assert!(
        r[6].text
            .starts_with("Path '../' escapes the workspace root '"),
        "{}",
        r[6].text
    );
    assert!(r[7].error);
    assert!(
        r[7].text
            .starts_with("invalid_glob: Invalid glob '[unclosed': "),
        "{}",
        r[7].text
    );
}

// ─────────────────────────────────── lsp ──────────────────────────────────

const LSP_STUB: &str = r#"let buf = Buffer.alloc(0);
function send(o) {
  const s = JSON.stringify(o);
  process.stdout.write('Content-Length: ' + Buffer.byteLength(s) + '\r\n\r\n' + s);
}
const range = (l, c) => ({ start: { line: l, character: c }, end: { line: l, character: c + 6 } });
function handle(m) {
  const uri = m.params && m.params.textDocument && m.params.textDocument.uri;
  if (m.method === 'initialize') {
    return send({ jsonrpc: '2.0', id: m.id, result: { capabilities: { definitionProvider: true, hoverProvider: true, referencesProvider: true, textDocumentSync: 1 } } });
  }
  if (m.method === 'textDocument/definition' || m.method === 'textDocument/hover' || m.method === 'textDocument/references') {
    if (uri && uri.endsWith('boom.rs')) {
      return send({ jsonrpc: '2.0', id: m.id, error: { code: -32603, message: 'stub-server-exploded' } });
    }
    if (m.method === 'textDocument/definition') return send({ jsonrpc: '2.0', id: m.id, result: [{ uri, range: range(0, 3) }] });
    if (m.method === 'textDocument/hover') return send({ jsonrpc: '2.0', id: m.id, result: { contents: { kind: 'markdown', value: 'HOVER-FROM-STUB' } } });
    return send({ jsonrpc: '2.0', id: m.id, result: [{ uri, range: range(0, 3) }, { uri, range: range(2, 4) }] });
  }
  if (m.method === 'textDocument/didSave') {
    return send({ jsonrpc: '2.0', method: 'textDocument/publishDiagnostics', params: { uri, diagnostics: [{ range: range(1, 2), severity: 1, message: 'stub-diagnostic-error' }] } });
  }
  if (m.method === 'shutdown') return send({ jsonrpc: '2.0', id: m.id, result: null });
  if (m.method === 'exit') process.exit(0);
  if (m.id !== undefined) send({ jsonrpc: '2.0', id: m.id, result: null });
}
process.stdin.on('data', (d) => {
  buf = Buffer.concat([buf, d]);
  for (;;) {
    const i = buf.indexOf('\r\n\r\n');
    if (i < 0) return;
    const m = /Content-Length: (\d+)/i.exec(buf.slice(0, i).toString());
    const n = parseInt(m[1], 10);
    if (buf.length < i + 4 + n) return;
    const body = JSON.parse(buf.slice(i + 4, i + 4 + n).toString());
    buf = buf.slice(i + 4 + n);
    handle(body);
  }
});
"#;

const LSP_FILES: &[(&str, &str)] = &[
    (
        "src/main.rs",
        "fn helper() {}\nfn main() {\n    helper();\n}\n",
    ),
    ("src/boom.rs", "fn boom_fn() {}\nfn call() { boom_fn(); }\n"),
    ("lsp-stub.js", LSP_STUB),
];

fn with_server(stub: &str, mut args: Value) -> Value {
    args["server_path"] = json!("node");
    args["server_args"] = json!([stub]);
    args
}

#[test]
fn lsp_tool_talks_to_a_stdio_language_server_and_falls_back_locally_when_it_fails() {
    let cell = RootCell::new();
    let stub = {
        let cell = cell.clone();
        move || {
            cell.root()
                .join("lsp-stub.js")
                .to_string_lossy()
                .to_string()
        }
    };
    let (s0, s1, s2, s3, s4, s5) = (
        stub.clone(),
        stub.clone(),
        stub.clone(),
        stub.clone(),
        stub.clone(),
        stub.clone(),
    );
    let p = sequence(
        vec![
            call_with("lsp", move |_| {
                with_server(
                    &s0(),
                    json!({"operation":"goto_definition","path":"src/main.rs","line":3,"character":5}),
                )
            }),
            call_with("lsp", move |_| {
                with_server(
                    &s1(),
                    json!({"operation":"references","path":"src/main.rs","line":3,"character":5}),
                )
            }),
            call_with("lsp", move |_| {
                with_server(
                    &s2(),
                    json!({"operation":"hover","path":"src/main.rs","line":3,"character":5}),
                )
            }),
            call_with("lsp", move |_| {
                with_server(
                    &s3(),
                    json!({"operation":"diagnostics","path":"src/main.rs"}),
                )
            }),
            // The server errors on boom.rs: definition degrades to the local symbol index...
            call_with("lsp", move |_| {
                with_server(
                    &s4(),
                    json!({"operation":"goto_definition","path":"src/boom.rs","line":2,"character":13}),
                )
            }),
            // ...but references have no local fallback and must say so.
            call_with("lsp", move |_| {
                with_server(
                    &s5(),
                    json!({"operation":"references","path":"src/boom.rs","line":2,"character":13}),
                )
            }),
            call("lsp", json!({"operation":"status"})),
            call("lsp", json!({"operation":"goto_definition"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, LSP_FILES);
    cell.bind(&rig);
    let out = rig.run("go");
    let r = expect_results(&out, 8);

    let defs = norm(&r[0].text);
    assert!(!r[0].error, "{defs}");
    assert!(defs.starts_with("Definitions:\nfile:///"), "{defs}");
    assert!(defs.ends_with("/src/main.rs:1:3"), "{defs}");

    let refs = norm(&r[1].text);
    assert!(refs.starts_with("References:\n"), "{refs}");
    let lines: Vec<&str> = refs.lines().skip(1).collect();
    assert_eq!(lines.len(), 2, "{refs}");
    assert!(lines[0].ends_with("/src/main.rs:1:3"), "{refs}");
    assert!(lines[1].ends_with("/src/main.rs:3:4"), "{refs}");

    assert!(!r[2].error, "{}", r[2].text);
    assert!(r[2].text.contains("HOVER-FROM-STUB"), "{}", r[2].text);

    assert!(!r[3].error, "{}", r[3].text);
    assert!(r[3].text.starts_with("Diagnostics:\n"), "{}", r[3].text);
    assert!(r[3].text.contains("stub-diagnostic-error"), "{}", r[3].text);
    assert!(
        norm(&r[3].text).contains("/src/main.rs:2:2"),
        "{}",
        r[3].text
    );

    assert!(!r[4].error, "{}", r[4].text);
    assert!(
        r[4].text.starts_with("[LSP server failed:"),
        "{}",
        r[4].text
    );
    assert!(r[4].text.contains("stub-server-exploded"), "{}", r[4].text);
    assert!(r[4].text.contains("using local fallback"), "{}", r[4].text);
    assert!(
        r[4].text.contains("Definitions for 'boom_fn':"),
        "{}",
        r[4].text
    );

    assert!(r[5].error);
    assert!(
        r[5].text.starts_with("LSP references failed:"),
        "{}",
        r[5].text
    );
    assert!(r[5].text.contains("stub-server-exploded"), "{}", r[5].text);

    assert!(!r[6].error, "{}", r[6].text);
    assert!(r[6].text.starts_with("workspace: "), "{}", r[6].text);
    assert!(
        r[6].text.contains("fallback: local symbol index"),
        "{}",
        r[6].text
    );
    assert!(r[7].error, "{}", r[7].text);
}
