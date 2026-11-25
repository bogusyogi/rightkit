# Changelog

## 0.3.3

- Released with the `rightkit-fs` 0.5 dependency already on main (the published previous version still required `rightkit-fs` ^0.4, so consumers carried two `rightkit-fs` versions; bogusyogi/pulse#643).

## 0.3.2 (2026-11-16)

- The package no longer ships `src/trusted-update-keys.json` (an empty key list nothing read). No public item, feature or behaviour changes; the compiled library is unchanged.

## 0.3.1 (2026-11-16)

- New `skeletonization::SkeletonVariant` (`Standard`, the default and today's behaviour, unchanged; `Extended`), `skeletonize_for_task_with` and `expand_region_with`, and the module `skeletonization_extended`. `Extended` is a host's newer skeletonizer: TSX parsed with the TSX grammar and `.mts`/`.cts` as TypeScript, imports kept verbatim, Python decorated definitions and JS/TS `export` wrappers unwrapped (the decorator or `export` text is kept), `union` items, Rust impls named `Trait for Type` (an impl is kept in full when its type or trait is task-relevant), `#2`, `#3`, ... suffixes on repeated region names, `expand_region` refusing the empty name, a first-line fallback for every other named top-level node, and a stricter relevance match (an unnamed item is never relevant; names and terms under three characters match only a whole identifier part). `skeletonize`, `skeletonize_for_task` and `expand_region` are unchanged. Every grammar `Extended` uses was already a dependency.

## 0.3.0

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-context-core` 0.1 -> 0.2, `rightkit-context-protocol` 0.1 -> 0.2, `rightkit-embed` 0.6 -> 0.7, `rightkit-onnx` 0.2 -> 0.3. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- Additive: `GenerateDocsOptions::dry_run` (also `docsDryRun` on the `build` request's input). A docs build with `dry_run` writes nothing, deletes nothing and creates no folder, and returns `mode: "dry_run"` with `GenerateDocsResult::plan`: for each file a real run would touch, in its order (the two docs, or the two `.agent/docs` fallbacks after a `docs_conflict`, whose `conflicts` and `fallback` are filled as in a real run; the README when the pointer is kept; `START-HERE.md` when it would be retired), a `DocAction` of `Created`, `Changed` (the bytes differ), `Unchanged` (the bytes are identical) or `Removed`. The README pointer's text is now built by one private function shared with `maintain_readme_pointer`, which behaves as before. A struct literal of `GenerateDocsOptions` or `GenerateDocsResult` written outside this crate needs `..Default::default()` (both implement `Default`). `docsResult` of the `build` response gains `plan` (empty unless a dry run). No version bump.
- `status` (and `db-status`) no longer fail with `blueprint_oversized` when the response is over its byte ceiling and no receipt listing is left to shrink (ME-5 step 3). The response is trimmed to the ceiling the way other operations already are: the largest collections are halved and each cut is recorded as a `byte_ceiling` omission with its count, then `sourceObservation`, `freshnessReceipt` and `detail` are cut if they are still too large (a long string is halved, anything else becomes `null`, each recorded in `omissions`). The result carries `truncated: true` and `byteCeiling`. The limit values are unchanged; a response that still cannot fit after every cut returns the same error as before. Receipt listings still shrink first, with their `...Count` and `...Omitted` counters. For Pulse, which pins `rightkit-repo-graph =0.3.0` (the published version; this source is 0.3.2): the change reaches Pulse only after the next publish of this crate, and Pulse then raises the pin to that version.
- Written, not yet compiled. No unit tests, per the 2026-11-03 rule.
- Brought up to the engine copy this crate was lifted from (#1572):
  - New modules: `graph_dir` (where a repository's graph lives: `set_state_root` moves it out of the work tree to `<state root>/blueprint/<name>-<hash>/graph/`; with no state root the `<repo>/.agent/graph/` location is unchanged), `html_map` (self-contained map page behind the `export` operation, writes `map.html` beside the store), `memory` (one whole-graph load at a time, freed pages handed back to the OS), and the internal `refresh_view`.
  - Refresh and load: a delta refresh reads only the changed files and their referrers, falls back to a byte-budgeted full build or a typed deferral, resets edges to vanished nodes to unresolved, and edges of a rewritten path are found through the source index instead of an `evidence LIKE` scan; the store never caches generations above 300k rows. New `scan_repository_digests_with_cancellation`, `watch::check_limits`, `graph::unresolve_edge`, `store` partial loaders (`stored_graph_rows`, `load_path_nodes`, `load_node_edges`, `load_path_file_report`), `BlueprintBounds::for_request` (a request's `maxBytes` bounds its response).
  - Answers: results of operations that did not fit themselves are cut to the request's byte bound with `byte_ceiling` omissions instead of being rejected; a late `search` page is an answer, not `deadline_exceeded`; search matches every whitespace-separated term and reports per-term counts; call-keywords (`if`, `for`, ...) no longer become unresolved CALLS edges.
  - Breaking: `framework_intelligence::FiFile` now borrows its text (`FiFile<'a>`, `text: &'a str`) and `FiFile` fields are otherwise unchanged. No caller inside RightKit uses it.
- Watcher: an event-batch overflow (native queue over `max_events`, or a snapshot diff over it) no longer fails the poll with `WatchError::EventOverflow`, which degraded the service and retired the watcher. A queue overflow forces one immediate snapshot reconcile; a snapshot diff over the limit is handed to the refresh callback as one `.git/HEAD` Modify event (the root-level "discover every change by digest" marker) and the new snapshot becomes the baseline. The watcher keeps running and the gap clears on success. `WatchError::EventOverflow` stays in the enum but `poll` no longer returns it.
- Refresh: a rename whose source is not in the stored graph, or whose source survives the event, now rebuilds the root (`full_build_or_defer`, trigger `rename source/destination is inconsistent`) instead of returning `blueprint_incremental_unsupported`. The check now runs before any delta is written, not after.
- `qa_env` is unchanged: its overrides stay compiled out (the engine copy has them behind a `qa-native` feature; not ported, see the open decision in the commit report).

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `membrane-blueprint` (engine).
- `skeletonization`: deterministic tree-sitter worker-input skeletons (`skeletonize_for_task`, `skeletonize`, `expand_region`, `DEFAULT_SIZE_THRESHOLD`), ported from CodeRight.
