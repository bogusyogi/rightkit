# Fresh RightKit cutover inventory

Read catalog.json. Inspect every row against assigned app, using current production source & current config only. Ignore earlier audit reports. User requests fresh perspective. Audit output only; no product changes, installs, builds, network, commits, publication, or messages to other chats. You are not alone: preserve other agents' edits. Own only apps/<slug>.json & apps/<slug>.md.

Read applicable AGENTS.md. Use Foundation source evidence rules (legion foundation skill). For Swift also read legion ios-development & macos-development SKILL.md under /Users/adrdsouza/.claude/skills/legion/skills/. Trace actual target/caller; no Apple builds needed. Read canonical owner source APIs before claiming app-local code duplicates them. Work in batches of no more than 25 components. One adversarial self-review: challenge every Y/M, irrelevant NA, runtime mismatch & inferred duplicate. No prior report is implementation evidence.

Y = relevant feature actually routes through RightKit, with no active competing app-owned implementation within owner's precise scope. Transitive backend usage counts if actual call chain is proven. N = relevant feature actively implemented locally, no shared production route. M = shared & old competing implementations both active (also across app targets when both applicable). NA = component irrelevant to this app/runtime/feature. NA is never a synonym for unreviewed. A mere dependency/import is not Y. Stale version, local path patch, unpublished package or unexecuted runtime test does NOT change Y to M; record separately in delivery_flags. Product schemas/policy/UX/adapters around shared primitives are not legacy by default. Do not demand Rust from Node/Python/Swift-only apps, Swift products from Tauri-only apps, or browser UI packages from native apps. Identify usable language bridge before classifying cross-language N. QA, release & build tooling components cover their actual scripts/config callers rather than production UI. Private build-control owner is internal, normally NA for apps. Dependency updater (@rightkit/update) differs from app runtime @rightkit/updates.

Each file must contain ALL catalog IDs once, with honest evidence. Expected JSON shape:
{
 "app":"Name", "root":"/absolute/path", "head":"...", "reviewed_at":"ISO timestamp", "targets":["desktop", "ios", ...],
 "scope_note":"Source scope & exclusions; no execution qualification implied.",
 "cells":[{
  "component_id":"cargo:rightkit-pdf", "status":"Y|N|M|NA", "rationale":"Precise component-level reason, target scope & native API compatibility.",
  "shared_evidence":[{"path":"/absolute/production/file", "line":12, "symbol":"actual callable integration", "detail":"caller -> shared mechanism"}],
  "local_evidence":[{"path":"/absolute/production/file", "line":12, "symbol":"legacy capability", "detail":"live caller & actual overlapping mechanism"}],
  "applicability_evidence":[{"path":"/absolute/production/manifest-or-source", "line":12, "detail":"host/feature evidence incl NA"}],
  "owner_evidence":[{"path":"/absolute/rightkit/source", "line":12, "detail":"shared capability API for N/M recommendations"}],
  "delivery_flags":["Local path patch / version / staging fact, if relevant"],
  "remaining_work":"Concrete cutover & removal task, or none; preserve app policy", "confidence":"high|medium|low"
 }],
 "searches":[{"pattern":"...", "scope":"source dirs searched", "result":"findings/absence"}],
 "self_review":"What classifications were challenged & corrected."
}

Y requires shared_evidence production caller. N requires local_evidence actual overlap plus owner_evidence. M requires both shared_evidence & local_evidence actual overlapping capability plus owner_evidence. NA requires applicability_evidence & precise exclusion, supported by source census/search. Grouped app-host evidence can repeat for incompatible-runtime rows, but don't blanket NA all Rust if app has Rust. A persisted feature not exposed by this owner may remain app-owned. If evidence genuinely unresolved, use status null + rationale & confidence low; do not fabricate NA. Aim to resolve such cells from source before returning.

Read all app-owned active targets: desktop backend/frontend; native iOS/watchOS/extensions/helpers; production engine/service; scripts/CI/release/QA where applicable. Ignore generated bundles, caches, build output, node_modules, target, vendored dependencies, examples/fixtures unless app is itself maintained template. A wrapper around shared work counts as shared, not mixed merely because wrapper exists. For @rightkit/pdf-ui vs rightkit-pdf assess frontend reader & backend engine separately; editing/document policy stays app-owned where owner does not provide it.

Write app markdown summary with applicable Y/N/M rows, concrete per-component tasks & target details. Keep full 97-row coverage in JSON, not a giant markdown dump. Record relevant source hashes or file timestamps when feasible; current checkouts may change during audit. Summarize counts & largest concrete N/M gaps when finished. Stop after one bounded adversarial review.
