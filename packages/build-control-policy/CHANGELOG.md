# Changelog

## 0.1.2-phase1

- The cargo, rustc, rustdoc and rustup PreToolUse guard now fires only when the tool would run locally on the Mac or Dell (RustRight managed hosts). Hosts that are neither (Linux, cloud sessions, CI) pass every command; the host test is the same win32/darwin platform test `packages/release/managed-roots.mjs` uses. Hook input may carry `platform` to override `process.platform`.
- Commands run through `ssh` or `plink` are remote: the argument string after the host is not an invocation here. Local parts are still inspected: other segments (`ssh h true; cargo build`), `$(...)` and backticks, `env X=1 cargo`, and `ProxyCommand`/`LocalCommand` options.
- Denial text says RustRight.
- Issue: Orthic-Labs/rightkit#293.
