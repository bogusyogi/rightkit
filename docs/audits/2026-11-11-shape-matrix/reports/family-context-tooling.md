# Family review: Context, memory & code intelligence; Foundation & tooling

Read-only review, 11 Nov. CodeRight source was available (`S/survey/coderight`). Legion source is not available, so `rightkit-code-audit`, `rightkit-apple-ops` and the Legion half of `rightkit-research` are judged on the package alone.

Depth: deep on memory, adapt, bench and ml-lab. Light on everything else. That includes `rightkit-context` and `rightkit-repo-graph`, which the brief's 5,000-line rule would have made deep; both are single-source Pulse code held verbatim, and only their boundaries were checked. Statements are verified by reading unless marked suspicion or not verified.

The survey trees and the RightKit crates both contain almost no `#[test]` functions, so test presence was not used as a signal.

## rightkit-code-audit

1. **Exists as:** `crates/rightkit-code-audit-engine`, `crates/rightkit-code-audit-commands`, `crates/rightkit-code-audit-provider-sdk`, `crates/rightkit-code-audit-qualification` (cargo).
2. **Feeds (planned lines):** legion 254,890.
3. **Basis today:** Legion legion-audit, CLI commands and legion-dev modules, moved 1 Nov.
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `gaps`. Four-crate split (provider SDK, engine, commands, qualification) is sound, but it could not be compared with Legion (source unavailable) and still carries Legion names and one dead file.
6. **Per app:** legion: gaps (not comparable; plan says 254,890 lines, crates hold about 150,000).
7. **Boundary problems:**
   - engine depends on rightkit-ownership, rightkit-agents, rightkit-agent-tools, rightkit-browser
   - rightkit-research depends on code-audit-provider-sdk only for an error type
   - 423 'legion' strings in engine src (paths such as legion-audit/src/wf_port)
8. **Before publication:**
   - delete or wire orphan native_providers/security/history_qualification_record.rs (408 lines, no mod declaration)
   - replace Legion names in paths, ids and messages or document them as fixed schema ids
   - explain the 100k-line difference from the plan (tests, fixtures or code not moved)
   - split 5,399-line commands/audit.rs and 6,423-line reasoning/ingest.rs

Depth: light. Confidence: low.

## rightkit-context

1. **Exists as:** `crates/rightkit-context`, `crates/rightkit-context-client`, `crates/rightkit-context-core`, `crates/rightkit-context-federation`, `crates/rightkit-context-protocol`, `crates/rightkit-context-provider-sdk`, `crates/rightkit-context-runtime` (cargo).
2. **Feeds (planned lines):** pulse 98,511.
3. **Basis today:** Pulse membrane engine crates, moved crate for crate.
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `gaps`. Pulse's seven-crate layout carried over one to one and is the reference; a second host can only use it as the external membrane binary, and the binary's client lives in rightkit-memory, not here.
6. **Per app:** pulse: fits (same code (54,026 of 60,591 distinctive lines found in the package)); coderight: gaps (uses it through the CLI client that sits in rightkit-memory::membrane).
7. **Boundary problems:**
   - rightkit-context-client (185 lines, resident socket) and rightkit-memory::membrane (1,087 lines, CLI) are two clients of one engine in two packages
   - 3,429-line context_telemetry.rs sits in rightkit-memory-store
   - context-runtime (62k lines) depends on 20 RightKit crates including adapt-taste, repo-graph, sync, onnx
8. **Before publication:**
   - move the membrane CLI client from rightkit-memory into rightkit-context-client
   - move context_telemetry out of memory-store into context-runtime or its own module crate
   - Pulse host files (engine/src/membrane_child.rs, membrane_enroll.rs, membrane_cmds.rs, 2,151 lines) need a supervisor API in the package

Depth: light. Confidence: medium.

## rightkit-repo-graph

1. **Exists as:** `crates/rightkit-repo-graph`, `crates/rightkit-repo-graph-grammars` (cargo).
2. **Feeds (planned lines):** pulse 37,279, coderight 600.
3. **Basis today:** Pulse membrane-blueprint plus CodeRight skeletonization as one added module.
4. **Side by side:**

CodeRight's `engine/crates/skeletonization/src/lib.rs` (585 lines) is `rightkit-repo-graph/src/skeletonization.rs` (390 lines): signatures-only rendering of large files for four languages, with its own `Lang` enum (line 14). Pulse's blueprint extraction covers 30+ languages through its own registry. Both use tree-sitter; they do not share a language table.

5. **Shape:** `gaps`. Pulse's graph is the package and fits Pulse; CodeRight's skeletonizer was added beside it with its own four-language table instead of using the graph's 30+ language extraction.
6. **Per app:** pulse: fits (same code); coderight: fits (skeletonization.rs is its code (200 of 201 distinctive lines)).
7. **Boundary problems:**
   - skeletonization.rs has a private Lang enum (Rust, Python, JS, TS) beside the graph's own language registry
   - repo-graph depends on rightkit-context-core and rightkit-context-protocol, so it cannot be used without the context protocol
   - pulls rightkit-embed and rightkit-onnx unconditionally
8. **Before publication:**
   - drive skeletonization from the graph's language registry and grammars crate
   - put embed/onnx behind a feature
   - state in README whether repo-graph is usable without the membrane resident

Depth: light. Confidence: medium.

## rightkit-memory

1. **Exists as:** `crates/rightkit-memory`, `crates/rightkit-memory-core`, `crates/rightkit-memory-store` (cargo).
2. **Feeds (planned lines):** pulse 14,843, coderight 9,759.
3. **Basis today:** memory-core and memory-store are Pulse cortex-core and cortex-store; rightkit-memory is CodeRight's client of Pulse's membrane CLI plus its mobile in-memory store.
4. **Side by side:**

Pulse (`membrane/engine/crates/cortex-core`, `cortex-store`) is the memory engine: tiers, planner, retriever, vector index, SQLite `MemDb`, temporal facts, FTS5 projection. It is in RightKit unchanged as `rightkit-memory-core` (6,123 lines) and `rightkit-memory-store` (8,640 lines).

CodeRight does not have a second engine. Its `engine/crates/memory/src/lib.rs` says the retrieval router and tier router "were deleted under context-membrane-memory rule 41" and that Membrane (Pulse) owns desktop ranking, tiering and retrieval. What CodeRight has is:
- `coderight-membrane-cli` (now `rightkit-memory/src/membrane.rs`): a child-process client of Pulse's `membrane` binary. Trait `MembraneBackend` (membrane.rs:202) covers `context_pull`, `memory_push/list/search/delete`, `blueprint_search`, `adapt_observe/recall/apply/taste_review`, `transcript_append/tip`. So it is the client of the whole context engine, not of memory only.
- `coderight-memory-backend` (now `rightkit-memory/src/backend`): trait `MemoryBackend` (contract.rs:68) with 16 async methods, implemented over that CLI. `native.rs:1-21` documents what is lost: name, scope, tier and keywords are not forwarded; `entries`, `get_full`, `activity_json`, `embedder_dim` return an explicit error; telemetry calls are no-ops.
- `memory/src/store.rs`: an in-memory `BTreeMap` store with write screening, used on mobile.

So the 99% and 62% figures are engine plus client, in separate crates, not two engines stacked. The defects are in how the client crate is cut:
- It is named `rightkit-memory` while the engine has the suffixed names, and it does not depend on either engine crate.
- `MemoryTier` and `MemoryEntry` are redeclared field for field (`rightkit-memory/src/backend/types.rs:76,103` against `rightkit-memory-core/src/types.rs:8,26`).
- `rightkit-memory/src/retriever.rs` (486 lines, an epsilon-greedy `RoutingPolicy` bandit) is on disk but not declared in `lib.rs`; `lib.rs` says the crate "ships no retrieval router". `rightkit-memory-core/src/routing.rs` already has `RoutingPolicy` and `RetrievalTier`. CodeRight deleted this file; RightKit kept it. The crate's `rightkit-ownership.json` still advertises "adaptive retrieval routing".
- `membrane.rs:420-460` hard-codes `/Applications/Pulse.app/Contents/Helpers/membrane`, `Programs/Pulse/Helpers/membrane.exe` and `notch-state.json`.
- Feature `agent_host` adds axum and four agent crates for `memory_save_tool`, `blueprint_tools`, `memory_provenance`, `api_memright`, which are CodeRight host routes and tools.

Best shape: `rightkit-memory-core` (pure) and `rightkit-memory-store` (SQLite) stay as they are. The CLI client moves to `rightkit-context-client`, which today holds only the 185-line resident client. `MemoryBackend` stays as the host-facing trait, using memory-core's types, in a crate whose name says client. Host tools move to `rightkit-agent-server`. Today's package is not that shape.

5. **Shape:** `wrong`. Not two competing engines: Pulse's engine and CodeRight's client of that engine. But the crate named rightkit-memory is the client, it does not use the engine crates' types, and it carries a dead second retrieval router.
6. **Per app:** pulse: fits (memory-core and memory-store are its code unchanged); coderight: gaps (backend and CLI client held; bin-side binding code (membrane_binding/*, api_memory.rs, about 3,000 lines) is in agent-server, not here).
7. **Boundary problems:**
   - MemoryTier and MemoryEntry declared twice (rightkit-memory/src/backend/types.rs:76,103 and rightkit-memory-core/src/types.rs:8,26)
   - rightkit-memory/src/retriever.rs (486 lines) is not declared in lib.rs and duplicates memory-core routing
   - membrane.rs is the client for context pull, blueprint, adapt and transcript commands, not only memory
   - membrane.rs:420-460 hard-codes Pulse.app install paths and notch-state.json
   - store.rs is a third, in-memory BTreeMap store for mobile
   - agent_host feature pulls axum and four agent crates into a memory crate
   - memory-store depends on rightkit-context-protocol
8. **Before publication:**
   - delete retriever.rs
   - move membrane.rs to rightkit-context-client; keep MemoryBackend trait and native backend as rightkit-memory-client or a feature of it
   - re-export MemoryTier/MemoryEntry from memory-core instead of redeclaring
   - make the membrane binary locations a host-supplied list
   - move agent_host modules (memory_save_tool, blueprint_tools, memory_provenance, api_memright) to rightkit-agent-server
   - decide whether rightkit-memory becomes a facade over core+store or is renamed

Depth: deep. Confidence: high.

## rightkit-adapt

1. **Exists as:** `crates/rightkit-adapt`, `crates/rightkit-adapt-taste` (cargo).
2. **Feeds (planned lines):** pulse 15,823, coderight 8,073.
3. **Basis today:** rightkit-adapt is CodeRight's coderight-learning crate; rightkit-adapt-taste is Pulse's membrane-adapt.
4. **Side by side:**

Two subsystems that share a word.

Pulse `membrane-adapt` (now `rightkit-adapt-taste`, 15,220 lines): governed learning from transcripts. Deterministic, no model, no network. Taste candidates from user transcript events, Insights from failures, signed manifests (`ring`), a single admission boundary into Cortex ("Adapt never owns a parallel durable store", lib.rs). Used by `rightkit-context-runtime`.

CodeRight `coderight-learning` (now `rightkit-adapt`, 3,349 lines, file for file; `proposals.rs` differs only in the crate prefix): post-session pipeline. `PostSessionLearningExtractor` to `EvaluationEngine` to `ProposalDeployer`; workflow optimisation proposals gated by `EvalGate` on held-out pass rate, safety, cost and latency (eval_gate.rs:8); skill optimisation epochs (`skill_opt.rs`, 854 lines); `harness_radar.rs` (532 lines), which turns external agent-harness watch-lists into "host app backlog proposals". Used by `rightkit-agent-server` and `rightkit-agent-engine-api-workflows`. Depends on `rightkit-agent-engine-storage`.

Differences in behaviour: Pulse learns user preferences and failure patterns and writes them as memory records through an admission gate; CodeRight proposes edits to workflows and skills and promotes them on benchmark evidence with a human `PromotionRecord`. No type is shared. There are three proposal types (`rightkit-adapt/src/types.rs` `Proposal`, `rightkit-adapt/src/proposals.rs:28` `Proposal` exported as `WorkflowProposal`, `rightkit-adapt-taste/src/proposal.rs`) and two in-memory proposal stores inside `rightkit-adapt` alone (`store.rs`, `proposal_store.rs`).

CodeRight also consumes Pulse's adapt: `rightkit-memory/src/membrane.rs:229-256` calls `adapt observe`, `adapt recall`, `adapt apply`, `adapt taste-review`, and CodeRight's `adapt_application_host.rs` and `api_adapt.rs` sit on that. So for the capability "adapt", CodeRight is already a client of Pulse's implementation; its own crate is a different capability (agent skill and workflow optimisation).

Best shape: `rightkit-adapt` is Pulse's crate. CodeRight's learning crate gets a name of its own next to `rightkit-agent-skills` and `rightkit-agent-orchestration` (which already holds `executed_bakeoff`). `harness_radar` is CodeRight's product. Today's package is not that shape.

5. **Shape:** `wrong`. Two unrelated subsystems under one name with no shared type and no dependency between them; the Pulse one (the reference) has the suffixed name.
6. **Per app:** pulse: fits (adapt-taste is its code unchanged (9,686 of 10,046 distinctive lines)); coderight: gaps (learning crate held verbatim; bin-side learning/* and adapt_application_host.rs are not in the package).
7. **Boundary problems:**
   - three Proposal types: rightkit-adapt types::Proposal, proposals::Proposal (exported as WorkflowProposal), adapt-taste proposal.rs
   - two in-memory proposal stores inside rightkit-adapt (store.rs LearningProposalStore, proposal_store.rs ProposalStore)
   - harness_radar.rs (532 lines) is CodeRight's product backlog feed
   - skill_opt, skill_synthesis, meta_skill, skill_bundle are agent-skill optimisation, closer to rightkit-agent-skills
   - eval_gate name used in rightkit-adapt and rightkit-memory-core for different things
   - CodeRight reaches Pulse's adapt through membrane CLI calls in rightkit-memory/src/membrane.rs:229-256
8. **Before publication:**
   - rename rightkit-adapt-taste to rightkit-adapt (Pulse governed learning is the reference)
   - move CodeRight learning to its own crate (for example rightkit-agent-learning) or into rightkit-agent-skills
   - move harness_radar back to CodeRight or to a neutral watch-list crate
   - collapse the two proposal stores

Depth: deep. Confidence: high.

## rightkit-research

1. **Exists as:** `crates/rightkit-research` (cargo).
2. **Feeds (planned lines):** postright 1,572, legion 10,090.
3. **Basis today:** Legion legion-research; no PostRight code.
4. **Side by side:**

PostRight `crates/engine/src/digest.rs:1-25` stores caller-supplied dated findings and turns them into draft calendar entries with slot reservation. `rightkit-research` (workflow.rs, source.rs, evidence.rs, receipt.rs) is a sources-claims-evidence pipeline from Legion. They share no behaviour; 19 of 996 distinctive PostRight lines are in the crate.

5. **Shape:** `gaps`. The crate is Legion's sources-claims-evidence pipeline; PostRight's digest code is a calendar-drafting feature mapped here by name and does not belong.
6. **Per app:** postright: wrong (digest.rs turns dated findings into calendar drafts (content-plan); reddit_reader is a source reader with no home in the crate); legion: gaps (not comparable).
7. **Boundary problems:**
   - PostRight digest.rs belongs in rightkit-content-plan
   - depends on rightkit-code-audit-provider-sdk for ProviderError (src/error.rs:53)
8. **Before publication:**
   - remap PostRight digest to rightkit-content-plan
   - decide whether reddit_reader becomes a source in rightkit-research::source or stays in media-fetch
   - remove the code-audit-provider-sdk dependency

Depth: light. Confidence: medium.

## rightkit-embed

1. **Exists as:** `crates/rightkit-embed` (cargo).
2. **Feeds (planned lines):** mailright 398.
3. **Basis today:** pre-existing RightKit crate (0.6.5).
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `best`. Embedder plus SQLite vector store; MailRight's 398 lines are a background job and Tauri command over it, which is app glue.
6. **Per app:** mailright: fits (semantic_job.rs is a job wrapper; nothing to add to the crate); pulse: fits (memory-core and context-runtime already depend on it).
7. **Boundary problems:**
   - MailRight semantic_job.rs maps better to rightkit-job-queue usage than to this crate
8. **Before publication:**
   - none

Depth: light. Confidence: medium.

## rightkit-search

1. **Exists as:** `crates/rightkit-search` (cargo).
2. **Feeds (planned lines):** mailright 944.
3. **Basis today:** pre-existing RightKit crate (0.4.5); holds MailRight's html-to-text.
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `best`. Query language, FTS5 builder and html_text are in place; MailRight's FTS index maintenance is schema-bound app code.
6. **Per app:** mailright: fits (fts_text.rs held (68 of 82 lines); fts_index.rs and fts_rebuild_job.rs are MailRight schema); coderight: fits (already pins rightkit-search 0.4.0).
7. **Boundary problems:**
   - grep feature (workspace content search, CodeRight port) is a different capability from query compilation
8. **Before publication:**
   - none

Depth: light. Confidence: medium.

## rightkit-vault

1. **Exists as:** `crates/rightkit-vault` (cargo).
2. **Feeds (planned lines):** pulse 1,984.
3. **Basis today:** fresh implementation of split-key secret sync; README says written without a build or test run.
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `gaps`. The crate is a different capability from the Pulse file mapped to it; Pulse's agent logins and payment box is already in rightkit-remote-gateway.
6. **Per app:** pulse: n/a (agent_secrets.rs is held in rightkit-remote-gateway/src/agent_secrets.rs, not here).
7. **Boundary problems:**
   - plan mapping wrong: Pulse agent_secrets (login.fill, payment.fill) is not split-key sync
8. **Before publication:**
   - remap Pulse agent_secrets to rightkit-remote-gateway
   - build and test the crate before publication (README states it never compiled)
   - depends on entitlement endpoints that must exist first

Depth: light. Confidence: medium.

## rightkit-channel-memory

1. **Exists as:** `crates/rightkit-channel-memory` (cargo).
2. **Feeds (planned lines):** postright 1,618.
3. **Basis today:** pre-existing RightKit contract crate (0.2.3).
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `best`. Schema, validation and write gate; PostRight already imports it and keeps only its SQLite revisions table.
6. **Per app:** postright: fits (memory.rs already uses rightkit_channel_memory); genright: fits (named consumer G7; not verified).
7. **Boundary problems:**
   - shares the word memory with rightkit-memory but is unrelated (content channel document)
8. **Before publication:**
   - none

Depth: light. Confidence: medium.

## rightkit-ml-lab

1. **Exists as:** `packages/ml-lab` (npm).
2. **Feeds (planned lines):** heardright 53,827.
3. **Basis today:** HeardRight training and evaluation scripts copied with their directory layout.
4. **Side by side:**

298 files, 51,521 lines: 158 Python, 106 `.mjs`, 23 PowerShell, 5 shell. All from HeardRight (`tauri-app-next/scripts`, `tauri-app-next/bakeoff`, `scripts`); the earlier match reported 0% only because it looked for a crate, and 36,064 of 36,247 distinctive lines are in `packages/ml-lab`.

Is it a shared package? No. The README says so: "Scripts shipped as files, not an importable library: nothing is built and there is no `main`." `package.json` has `"private": true`, no `bin`, no `exports`, no dependencies. Nothing in RightKit imports it.

Is it publishable? No.
- 60 files contain machine paths: `D:\Claude` 86 times (for example `scripts/whisper_bucket_eval.py:27`, `tools/zephyr_delete_safe_remaining_raws.ps1:7`), `E:\heardright_archive`, `E:\models`, `/Volumes/D`, and a personal home directory.
- 183 files refer to HeardRight; scripts read `../../heardright-engine/src/l3_cleanup_sections/section01.rs` and `../../../docs/results/...`, which are not in the package.
- No `requirements.txt` or `pyproject.toml` for scripts that import torch and numpy.
- Contents are experiments, not tools: `bakeoff/sotto-vast-canonical-20260727`, `bakeoff/l1-v13_2-backtrack-v1`, 82 files of `local-model-training` for one polish model, 40-odd `tools/zephyr_*` scripts for one wake-word corpus, including PowerShell that deletes raw training data on one machine.
- No secret was found by pattern search (not a full audit).

Reusable parts are small: `scripts/open-asr-bakeoff` (adapter-per-model harness, `score-asr-wer.mjs`, `report-openasr.mjs`, about 1,500 lines plus adapters) fits the bench receipt shape and overlaps ScrapeRight's ASR claim cases; `scripts/coreml` conversion scripts serve `rightkit-asr`'s CoreML models.

5. **Shape:** `wrong`. A private, unbuilt file dump of one product's experiments: no entry points, no dependency manifest, machine paths, dated experiment folders. Not a shared package and not publishable.
6. **Per app:** heardright: wrong (moving the scripts here gives nothing; they still refer to heardright-engine source files and D:\Claude\heardright paths).
7. **Boundary problems:**
   - 60 files hold machine paths (D:\Claude 86 times, E:\heardright_archive, /Users/<name>)
   - 183 files refer to heardright; schema ids heardright.l1.*
   - scripts read ../../heardright-engine/src/*.rs which is not in the package
   - 23 PowerShell scripts delete or quarantine training data on a specific machine
   - no requirements.txt or pyproject for 158 Python files (torch, numpy)
   - score-asr-wer.mjs and the ASR adapter harness overlap the bench receipt shape
   - plan lists it as a crate; it is an npm package marked private
8. **Before publication:**
   - do not publish
   - keep experiments (bakeoff/*, tools/zephyr_*, local-model-training) in a HeardRight research repository
   - extract only reusable pieces: WER scoring and ASR adapter harness to the bench package, CoreML/ONNX conversion to a model-tools package with a pinned Python manifest

Depth: deep. Confidence: high.

## rightkit-bench

1. **Exists as:** `crates/rightkit-bench` (cargo).
2. **Feeds (planned lines):** scraperight 15,276, mailright 4,191, viewright 2,725, coderight 5,329.
3. **Basis today:** MailRight timing helpers and ScrapeRight article scorer, plus fresh load driver and real-database probes.
4. **Side by side:**

What each app's bench code does:
- MailRight (4,191 lines Rust): `benchmarks.rs` has `BenchMetric`, `percentile_ms`, `measure_many` (lines 6-49), now `rightkit-bench/src/timing.rs` almost verbatim (the package returns `Err` where MailRight panics). `perf_proof/` seeds a deterministic 100k-message mailbox through the real apply path into a temporary SQLCipher database, then asserts `EXPLAIN QUERY PLAN` index names, runs a rekey round trip, and 15 probe files measure FTS, vectors, compaction and concurrency (`perf_proof/mod.rs:1-30`). `bin/load_driver.rs` is a concurrent load driver.
- ScrapeRight (about 1,140 lines of code; the other 14,135 planned lines are eight golden HTML pages): `benchmarks/article-quality/src/main.rs` scores extraction against goldens, now `rightkit-bench/src/article.rs`. `scripts/benchmark-receipts.mjs` reads a manifest of cases, evaluates each, writes `benchmark-receipt.json` and `.md`. `scripts/benchmark-claim-gate.mjs` refuses a public claim unless the manifest has at least 10 ASR and 30 OCR cases with named comparator systems. Swift and Python OCR and PDF probes.
- ViewRight (2,725 lines): `scripts/run-pdf-bench-matrix.mjs` runs fixtures by modes by repeats through the app, writes an aggregate receipt to a fresh path, and has `--print-plan` and `--validate <receipt.json>`. `run-mac-association-matrix.mjs` snapshots LaunchServices into a receipt with `--validate`. `src-tauri/src/pdf_bench.rs` is in-app counters (range requests, bytes read, process RSS) exposed as Tauri commands.
- CodeRight (5,329 lines): `api_benchmarks/` is HTTP routes for agent bake-offs, corpus bake-offs, run comparison and soak tests. It is held in `rightkit-agent-server/src/api_benchmarks` (2,301 of 2,356 distinctive lines found in RightKit, 13 in rightkit-bench).

One shared shape does exist, and three of the four apps follow it: a manifest of cases, a runner that produces one result row per case, a receipt written as JSON and Markdown to a fresh path, and a validator or gate that reads the receipt and passes or fails. `rightkit-bench` has none of it. It has timing, a load driver, SQLite plan checks and one domain scorer. The low line match has three causes: fixtures counted as code, bench code written in Node, and CodeRight's code living in another crate.

The package also states "no synthetic corpus exists in this crate" and gates writes on an armed `rightkit-qa` snapshot of the owner's real database (`owner.rs:31-56`). MailRight's proof is the opposite by design: hermetic, seeded, temporary database, "no real data and no opt-in". MailRight cannot move its harness in under the current rule.

`article.rs:151` `readable_text` is a second copy of `rightkit-web-extract/src/parse.rs:47`.

5. **Shape:** `gaps`. Holds timing primitives only. The shape the apps share (case manifest, run, receipt JSON and Markdown, validate or gate) is not in it, and three of four apps' bench code is Node or agent-server code that a Rust crate cannot hold.
6. **Per app:** viewright: gaps (matrix runners are .mjs on @rightkit/qa; need an npm receipt/validate library; pdf_bench.rs counters are ViewRight's); mailright: wrong (perf_proof seeds a 100k synthetic mailbox; crate states no synthetic corpus and real data only); scraperight: gaps (article scorer held; receipts and claim-gate scripts are not); coderight: n/a (agent bake-off and soak routes are held in rightkit-agent-server/src/api_benchmarks).
7. **Boundary problems:**
   - 14,135 of ScrapeRight's 15,276 planned lines are golden HTML fixtures, not code
   - article.rs readable_text duplicates rightkit-web-extract/src/parse.rs:47
   - CodeRight api_benchmarks is agent evaluation over HTTP, already in rightkit-agent-server and rightkit-agent-orchestration
   - ViewRight association matrix is a LaunchServices QA harness, closer to @rightkit/qa
   - real-data-only rule (owner.rs WriteGate) is a policy that excludes MailRight's hermetic seeded proof
8. **Before publication:**
   - add the shared shape: Manifest{cases}, CaseResult, Receipt (JSON + Markdown), validate(receipt), Gate(thresholds)
   - ship the same receipt schema as an npm module (@rightkit/qa/bench or @rightkit/bench) for the .mjs runners
   - remove readable_text and depend on rightkit-web-extract
   - allow a seeded temporary database path alongside owner probes
   - remap CodeRight api_benchmarks to rightkit-agent-server

Depth: deep. Confidence: high.

## rightkit-engine-host

1. **Exists as:** `crates/rightkit-engine-host` (cargo).
2. **Feeds (planned lines):** postright (no inventory rows), scraperight (no inventory rows), mailright (no inventory rows), genright (no inventory rows), cutright (no inventory rows), pulse (no inventory rows), heardright (no inventory rows), legion (no inventory rows).
3. **Basis today:** framework pieces of GenRight's engine and protocol.
4. **Side by side:**

`EngineError {code, message, details?}` in `rightkit-engine-host/src/wire.rs:20` equals GenRight (`genright-protocol/src/lib.rs:369`) and ScrapeRight (`crates/protocol/src/engine.rs:47`). CutRight's shell-side type has no `details` (`engine_supervisor.rs:23`). MailRight has an enum (`engine_api/mod.rs:83`) plus a second struct with `&'static str` codes (`engine/src/envelope.rs:9`). PostRight, Pulse and HeardRight were not compared. No inventory file assigns any file to this package, so the plan's 19,720 lines cannot be traced.

5. **Shape:** `gaps`. Command registry served as CLI and stdio sidecar is the right shape and its error envelope matches GenRight and ScrapeRight; it was built from a low-priority app and not checked against HeardRight or Pulse, and it carries a video-project feature.
6. **Per app:** genright: fits (donor); scraperight: fits (EngineError {code,message,details} identical (crates/protocol/src/engine.rs:47)); cutright: gaps (its EngineError has no details field); mailright: gaps (engine_api uses an enum error and a second envelope with static codes); postright: gaps (not verified); heardright: gaps (not verified; no inventory rows); pulse: gaps (not verified; no inventory rows).
7. **Boundary problems:**
   - project.rs (505 lines, feature project-cmd) ties the host to rightkit-video-project, timeline and transcript
   - doctor.rs report duplicates rightkit-doctor/src/report.rs
   - apps.rs installed-app discovery is a suite launcher concern
   - plan gives 19,720 lines from 8 apps but no inventory file has a row with this target
8. **Before publication:**
   - move project.rs to rightkit-video-project
   - keep one doctor report (in rightkit-doctor) and delete engine-host/src/doctor.rs
   - produce per-app inventory rows so adoption can be measured
   - check HeardRight and Pulse engine entry points against the registry before publication

Depth: light. Confidence: low.

## rightkit-apple-ops

1. **Exists as:** `crates/rightkit-apple-ops` (cargo).
2. **Feeds (planned lines):** legion 11,306.
3. **Basis today:** Legion legion-apple, moved 1 Nov.
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `gaps`. Self-contained and free of Legion names; judged alone because Legion source is unavailable. One crate mixes App Store Connect API, build analysis, profiling and a 2,857-line mobile module.
6. **Per app:** legion: gaps (not comparable).
7. **Boundary problems:**
   - App Store Connect client overlaps release tooling in packages/release (suspicion, not verified)
   - mobile.rs is not described in the crate summary
8. **Before publication:**
   - confirm no overlap with packages/release upload code
   - split App Store Connect client from build analysis and profiling if a second consumer needs only one

Depth: light. Confidence: low.

## rightkit-doctor

1. **Exists as:** `crates/rightkit-doctor` (cargo).
2. **Feeds (planned lines):** coderight 1,652, postright 338, cutright 177, mailright 95, genright (no inventory rows), legion (no inventory rows).
3. **Basis today:** fresh framework shaped on the engine doctor rows the apps share.
4. **Side by side:**

CutRight (`crates/cutright-engine/src/doctor.rs:12`) and MailRight (`src-tauri/engine/src/doctor.rs:28`) both build `{"id","required","status","detail"}` rows and `readiness: {status: ready|not_ready, checks}`. `rightkit-doctor/src/row.rs` is that row and roll-up. One difference: MailRight is ready only when every row is ok (line 75); CutRight and the crate only require the required rows (line 132). PostRight adds child-process and API-version probes (not compared line by line). CodeRight has a larger dimension model (`doctor_dimensions.rs`, `doctor_dimension_probes.rs`) of which 63 distinctive lines are in the crate.

5. **Shape:** `gaps`. Row {id, required, status, detail} and the readiness roll-up are exactly what CutRight and MailRight hand-roll; CodeRight's dimension probes are not in it and the report exists twice.
6. **Per app:** cutright: fits (same check JSON (crates/cutright-engine/src/doctor.rs:12,139)); mailright: fits (same check JSON (src-tauri/engine/src/doctor.rs:28,84)); postright: gaps (child-process and API-version probes; not compared in detail); coderight: gaps (doctor_dimension_probes.rs and doctor_impl.rs (790 lines) mostly held elsewhere, 63 lines here).
7. **Boundary problems:**
   - engine doctor report built in both rightkit-doctor/src/report.rs and rightkit-engine-host/src/doctor.rs
   - clock.rs lifecycle lines and clipboard copy are not doctor concerns
   - MailRight readiness requires every row ok; crate requires only required rows
8. **Before publication:**
   - single report builder
   - decide where CodeRight's doctor dimensions live
   - move clipboard copy to rightkit-clipboard

Depth: light. Confidence: medium.

## @rightkit/config

1. **Exists as:** `packages/config` (npm).
2. **Feeds (planned lines):** heardright 159, viewright 304, pulse 58, scraperight 171, mailright 114, postright 101, genright 96, cutright 57, coderight 4.
3. **Basis today:** fresh presets (tsconfig, eslint, vite, Rust toml files).
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `best`. Small preset package; the app files mapped to it are near-identical deny, audit, about, toolchain and vite files.
6. **Per app:** heardright: fits (8 preset files); viewright: gaps (275-line vite.config.ts has app-specific build logic that stays in the app); pulse: fits; scraperight: fits; mailright: fits; postright: fits; genright: fits; cutright: fits; coderight: fits.
7. **Boundary problems:**
   - cargo toml presets (deny, audit, rust-toolchain) cannot be imported, only copied; needs a sync check (suspicion, mechanism not verified)
8. **Before publication:**
   - none

Depth: light. Confidence: medium.

## @rightkit/waitlist-worker

1. **Exists as:** `packages/waitlist-worker` (npm).
2. **Feeds (planned lines):** viewright 218, mailright 311.
3. **Basis today:** config-driven factory generalising the ViewRight and MailRight workers.
4. **Side by side:**

ViewRight `workers/waitlist/src/index.ts` (196 lines) and MailRight's (311 lines) share KV storage, per-IP hourly rate limit, IP hash with secret, Resend confirmation and the same error codes. MailRight adds `current_tool` and `biggest_frustration` merged into an existing signup (lines 76-86). The package's `WaitlistConfig` (index.ts:49) expresses both through `detailFields` and `recordSource`. Response bodies were not compared byte for byte.

5. **Shape:** `best`. createWaitlistWorker(config) covers both: ViewRight's source field (recordSource) and MailRight's two free-text fields (detailFields), same KV, rate limit, IP hash and Resend bindings.
6. **Per app:** viewright: fits (record fields and bindings match); mailright: fits (current_tool and biggest_frustration become detailFields).
7. **Boundary problems:**
   - none found
8. **Before publication:**
   - none

Depth: light. Confidence: medium.

## rightkit-ownership

1. **Exists as:** `crates/rightkit-ownership` (cargo).
2. **Feeds (planned lines):** none; not in the master plan.
3. **Basis today:** RightKit's own scanner, Rust port of the Node scanner in packages/release.
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `best`. RightKit infrastructure, not fed by any app and not in the master plan; no app shape to reconcile.
6. **Per app:** n/a.
7. **Boundary problems:**
   - rightkit-code-audit-engine depends on it
8. **Before publication:**
   - none

Depth: light. Confidence: low.

## rightkit-connector-gen

1. **Exists as:** `crates/rightkit-connector-gen` (cargo).
2. **Feeds (planned lines):** none; not in the master plan.
3. **Basis today:** fresh, std-only generator from an engine contract.
4. **Side by side:**

Single implementation, or not compared (light read).

5. **Shape:** `best`. 528 lines, no app code feeds it and it is not in the master plan; its input is the rightkit-job-queue method list.
6. **Per app:** n/a.
7. **Boundary problems:**
   - takes its own Contract type instead of rightkit-engine-host's contract export (suspicion)
8. **Before publication:**
   - none

Depth: light. Confidence: low.

## Family summary

19 packages: best 7, gaps 9, wrong 3, missing 0.

| Package | Shape | Depth | Confidence |
|---|---|---|---|
| `rightkit-code-audit` | gaps | light | low |
| `rightkit-context` | gaps | light | medium |
| `rightkit-repo-graph` | gaps | light | medium |
| `rightkit-memory` | wrong | deep | high |
| `rightkit-adapt` | wrong | deep | high |
| `rightkit-research` | gaps | light | medium |
| `rightkit-embed` | best | light | medium |
| `rightkit-search` | best | light | medium |
| `rightkit-vault` | gaps | light | medium |
| `rightkit-channel-memory` | best | light | medium |
| `rightkit-ml-lab` | wrong | deep | high |
| `rightkit-bench` | gaps | deep | high |
| `rightkit-engine-host` | gaps | light | low |
| `rightkit-apple-ops` | gaps | light | low |
| `rightkit-doctor` | gaps | light | medium |
| `@rightkit/config` | best | light | medium |
| `@rightkit/waitlist-worker` | best | light | medium |
| `rightkit-ownership` | best | light | low |
| `rightkit-connector-gen` | best | light | low |

Main findings:

- **Memory is engine plus client, cut along the wrong line.** Pulse's engine is `rightkit-memory-core` and `-store`. The crate named `rightkit-memory` is CodeRight's client of Pulse's `membrane` binary. It redeclares the engine's types, hard-codes Pulse install paths, keeps a 486-line retrieval router that is not compiled, and its CLI client also serves context, blueprint, adapt and transcript commands.
- **Adapt is two unrelated subsystems.** `rightkit-adapt-taste` is Pulse's governed learning (the reference). `rightkit-adapt` is CodeRight's skill and workflow optimisation pipeline plus a product backlog feed. No shared type; three proposal types. CodeRight already uses Pulse's adapt through the CLI.
- **Bench has primitives but not the shared shape.** ScrapeRight, ViewRight and CodeRight all do manifest, run, receipt, validate/gate; the crate has none of that. Most app bench code is Node scripts, fixtures or agent-server routes that this Rust crate cannot hold. Its real-data-only rule excludes MailRight's seeded proof.
- **ml-lab is not a package.** It is HeardRight's experiment scripts with machine paths, no manifest and no entry points, marked private. It should leave RightKit except for the WER/ASR bake-off harness and model conversion scripts.
- **Three plan mappings are wrong by name:** Pulse `agent_secrets` to `rightkit-vault` (it is in `rightkit-remote-gateway`), PostRight `digest` to `rightkit-research` (it is content planning), CodeRight `api_benchmarks` to `rightkit-bench` (it is in `rightkit-agent-server`).
- **Duplicates inside RightKit:** engine doctor report in `rightkit-doctor` and `rightkit-engine-host`; `readable_text` in `rightkit-bench` and `rightkit-web-extract`; `MemoryTier`/`MemoryEntry` in `rightkit-memory` and `rightkit-memory-core`; two engine clients (`rightkit-context-client`, `rightkit-memory::membrane`).
- **Not verified:** anything about Legion; `rightkit-engine-host` against HeardRight, Pulse and PostRight; internals of `rightkit-context-runtime`; `rightkit-vault` has never been compiled according to its own README.
