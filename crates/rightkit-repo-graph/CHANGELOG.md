# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-context-core` 0.1 -> 0.2, `rightkit-context-protocol` 0.1 -> 0.2, `rightkit-embed` 0.6 -> 0.7, `rightkit-onnx` 0.2 -> 0.3. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- Brought up to the engine copy this crate was lifted from (#1572):
  - New modules: `graph_dir` (where a repository's graph lives: `set_state_root` moves it out of the work tree to `<state root>/blueprint/<name>-<hash>/graph/`; with no state root the `<repo>/.agent/graph/` location is unchanged), `html_map` (self-contained map page behind the `export` operation, writes `map.html` beside the store), `memory` (one whole-graph load at a time, freed pages handed back to the OS), and the internal `refresh_view`.
  - Refresh and load: a delta refresh reads only the changed files and their referrers, falls back to a byte-budgeted full build or a typed deferral, resets edges to vanished nodes to unresolved, and edges of a rewritten path are found through the source index instead of an `evidence LIKE` scan; the store never caches generations above 300k rows. New `scan_repository_digests_with_cancellation`, `watch::check_limits`, `graph::unresolve_edge`, `store` partial loaders (`stored_graph_rows`, `load_path_nodes`, `load_node_edges`, `load_path_file_report`), `BlueprintBounds::for_request` (a request's `maxBytes` bounds its response).
  - Answers: results of operations that did not fit themselves are cut to the request's byte bound with `byte_ceiling` omissions instead of being rejected; a late `search` page is an answer, not `deadline_exceeded`; search matches every whitespace-separated term and reports per-term counts; call-keywords (`if`, `for`, ...) no longer become unresolved CALLS edges.
  - Breaking: `framework_intelligence::FiFile` now borrows its text (`FiFile<'a>`, `text: &'a str`) and `FiFile` fields are otherwise unchanged. No caller inside RightKit uses it.
- `qa_env` is unchanged: its overrides stay compiled out (the engine copy has them behind a `qa-native` feature; not ported, see the open decision in the commit report).

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `membrane-blueprint` (engine).
- `skeletonization`: deterministic tree-sitter worker-input skeletons (`skeletonize_for_task`, `skeletonize`, `expand_region`, `DEFAULT_SIZE_THRESHOLD`), ported from CodeRight.
