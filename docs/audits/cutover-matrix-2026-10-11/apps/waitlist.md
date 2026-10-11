# Waitlist RightKit cutover

Fresh source-only audit at `22d78df2e5c9849ff97d566284acabdfc169de2d` (2026-10-11T08:15:05+05:30). Scope is one Cloudflare Workers TypeScript request handler, Workers KV bindings, & Wrangler scripts; subscriber KV values were not read.

Result: 0 Y, 0 N, 0 M, 97 NA (57 Cargo, 25 npm, 15 Swift).

All rows are NA from target compatibility & active feature scope. Production code accepts HTTP requests, uses native Workers Fetch/Web Crypto, persists to `WAITLIST`/`RATELIMIT` KV, & best-effort calls Listmonk. It has no Cargo, Swift, desktop, Tauri, browser-renderer, sidecar, test, release-artifact, dependency-update, or RightKit caller. Cloudflare Fetch & KV remain native host mechanisms; no forced desktop adoption applies.

Adversarial review specifically checked Rust HTTP against live Listmonk `fetch`, local `console.error` against Tauri-only `@rightkit/logs`, & QA/release/update/git/build-control packages against actual Wrangler scripts. None exposes a compatible active route.

Full 97-row evidence, source census, & self-review: [waitlist.json](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/waitlist.json:1).

