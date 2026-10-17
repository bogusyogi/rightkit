# Changelog

## Unreleased

- PostRight base dependency allowlist (Orthic-Labs/rightkit#362, #310): added `cargo:serde` (crates/protocol). The other 24 listed dependencies were already in the PostRight entry or the global list (`cargo:getrandom`). Takes effect only once a protected trust copy carries the allowlist; it is advisory until then. Not built or run here.

## 0.1.3-phase1

- The public-repository rule (`githubActionsOnlyViolation`) follows the same host gate: off the Mac and Dell it is skipped in the hook decisions.
- `rustright` with `cargo`, `rustc` or `rustdoc` is the sanctioned managed form, admitted exactly as `rightkit` is; `rightkit` stays admitted during the shim transition. Both are also admitted as `agent-bin/<name>[.cmd|.exe]` paths.
- Hook tag and messages say RustRight and suggest `rustright cargo ...`.

## 0.1.2-phase1

- The cargo, rustc, rustdoc and rustup PreToolUse guard now fires only when the tool would run locally on the Mac or Dell (RustRight managed hosts). Hosts that are neither (Linux, cloud sessions, CI) pass every command; the host test is the same win32/darwin platform test `packages/release/managed-roots.mjs` uses. Hook input may carry `platform` to override `process.platform`.
- Commands run through `ssh` or `plink` are remote: the argument string after the host is not an invocation here. Local parts are still inspected: other segments (`ssh h true; cargo build`), `$(...)` and backticks, `env X=1 cargo`, and `ProxyCommand`/`LocalCommand` options.
- Denial text says RustRight.
- Issue: Orthic-Labs/rightkit#293.
