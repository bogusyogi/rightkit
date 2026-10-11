# Architecture skeptic review — 2026-10-11

Independent adversarial review of the three actions in `architecture-review.md`. Scope is limited to that fresh evidence file and cited live source. No prior audit was used; no tests, builds, installs, network calls, product edits, or Blueprint graph were available.

F1 is confirmed as an ownership and contract contradiction, not proof that runners should merge. `packages/qa/README.md:7-8` places Node/browser tooling beside “None declared” for a Rust pair, while `crates/rightkit-qa/src/lib.rs:3-12` says it replaces `@rightkit/qa` and describes native app/control plus optional Chrome targets. `packages/qa/src/index.ts:1-12` exports a broader Node surface. Keep boundaries pending an API/platform/consumer matrix.

F2 is confirmed as compatibility debt. `crates/rightkit-qts-ggml/src/lib.rs:1-7` directs users to `rightkit-qts-ggml-sys` and forbids new API, and `crates/rightkit-qts/Cargo.toml:41-42` depends on the sys crate directly. Repository search found no in-tree dependency on the deprecated wrapper; external consumers still require a compatibility window before removal.

F3 is confirmed as a shared-contract candidate, not a proven removable duplicate. `rightkit-send` writes trust state through its local helper (`crates/rightkit-send/src/lib.rs:215-237`; caller at `crates/rightkit-send/src/trust.rs:244-252`). `rightkit-fs` exposes private, parent-creation, no-follow, mode-preserving, replacement, retry, and directory-sync behavior (`crates/rightkit-fs/src/atomic.rs:21-29`, `:175-237`, `:260-287`). A delegation decision needs those semantics checked across Unix and Windows.

Packet coverage is incomplete: 29 of 1,996 paths were examined and 1,967 were omitted under the packet byte cap. Findings therefore remain anchored to cited live source and `complete` stays false. No count or size was used as an architecture finding.
