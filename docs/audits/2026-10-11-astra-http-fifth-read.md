Reviewed committed `25fe6c337a3eacb5c4fd8cbcafb45fbbcca5c8c7` only. No builds, tests or working-tree changes. Process finding 6 remains **OPEN-IN-PR** (`win/process-spawn-handles`), excluded from HTTP findings.

| Fourth-read finding | Status | Committed evidence |
|---|---|---|
| 1 — Hard-linked partial corruption | **CLOSED** | Opened-handle link count checked before mutation: [download.rs:685](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:685). |
| 2 — Active lock replacement | **PARTLY** | Windows trailing-dot/space aliases rejected, but Unicode aliases survive ASCII folding: [download.rs:799](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:799), [download.rs:805](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:805). Finding 9 below. |
| 3 — Changed-resource `416` | **CLOSED** | Resource comparison retains query: [download.rs:488](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:488), [download.rs:595](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:595). |
| 4 — OAuth credential echo | **CLOSED** | Token exchange & device polling register secrets; provider errors scrub values: [oauth/lib.rs:526](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/lib.rs:526), [oauth/lib.rs:595](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/lib.rs:595), [device.rs:199](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/device.rs:199). |
| 5 — Windows tests silently skip | **PARTLY** | Link-fixture failures now panic, but replacement regression still returns success when rename fails: [download.rs:1367](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1367). Non-blocking coverage gap. |
| 6 — Same-URL shrinkage | **PARTLY** | Unproven prefixes restart & contradictory totals restart; `416` still ignores contradictory validators: [download.rs:380](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:380), [download.rs:483](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:483). Finding 10. |
| 7 — Scheme-scoped proxy bypass | **PARTLY** | Scope widening stopped, but scoped entries are silently discarded rather than rejected: [system_proxy.rs:121](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:121). Finding 11. |
| 8 — Per-scheme proxy endpoints | **CLOSED** | Different HTTP/HTTPS endpoints produce typed error through shared resolver: [client.rs:327](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:327), [async_client.rs:155](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:155). Fail-closed is safe; deferred routing is acceptable. |

Inherited numbered findings:

| Finding | Status | Evidence |
|---|---|---|
| Second 1 — Windows reparse refusal | **CLOSED** | [download.rs:854](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:854) |
| Second 2 — Concurrent corruption | **PARTLY** | Lock acquired at [download.rs:289](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:289); finding 9 bypasses exclusion. |
| Second 3 — Encoding observation | **CLOSED** | [wire.rs:93](/Volumes/D/claude/rightkit/crates/rightkit-http/src/wire.rs:93), [download.rs:604](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:604) |
| Second 4 — Silent async cancellation | **CLOSED** | [async_client.rs:788](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:788) |
| Second 5 — Retry sleep overrun | **CLOSED** | [client.rs:1195](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:1195) |
| Second 6 — OAuth regression unreachable | **CLOSED** | [oauth/e2e.rs:307](/Volumes/D/claude/rightkit/crates/rightkit-oauth/tests/e2e.rs:307) |
| First 1 — ALPN | **CLOSED** | [async_client.rs:114](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:114) |
| First 2 — Bundled roots | **CLOSED** | [async_client.rs:102](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:102) |
| First 3 — Proxy/policy routing mismatch | **CLOSED** | [async_client.rs:596](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:596) |
| First 4 — Short range completion | **CLOSED** | [download.rs:328](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:328) |
| First 5 — Async drain budgets | **CLOSED** | [async_client.rs:381](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:381) |
| First 6 — Download encoding refusal | **CLOSED** | [download.rs:603](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:603) |
| First 7 — Range overflow | **CLOSED** | [download.rs:1064](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1064) |
| First 8 — Response debug secrets | **CLOSED** | [client.rs:761](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:761), [async_client.rs:249](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:249) |

**Class-level verdict:** Centralization is incomplete. Production truncation uses `restart_from_zero`; writes use `append_part`; destination validation precedes filesystem access. Windows read/write handles plus seek-before-write correctly support truncation—consistent with [SetEndOfFile’s write-access requirement](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-setendoffile). However, representation checks remain duplicated across `206`/`416`, with different protections. Proxy resolution is shared, but scoped bypasses evade rejection; invalid single-proxy construction also becomes fallback through `.ok()` at [client.rs:322](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:322).

**BLOCKING — newly identified residuals & regression**

**9. P1 — Unicode destination alias can replace active lock, macOS.**  
[download.rs:805](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:805) checks ASCII lowercase only. `out.locK` passes, while default APFS treats it as `out.lock`: `K` canonically maps to `K`, then case folds. Staging names remain distinct; publication at [download.rs:353](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:353) replaces held lock, allowing another downloader into protected partial file.

This guard is wrong against [APFS normalization semantics](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/FAQ/FAQ.html) & [Unicode’s Kelvin-sign equivalence](https://www.unicode.org/versions/latest/ch03.pdf). **Fix:** validate filesystem-equivalent normalized/folded suffixes centrally, or isolate internal files from destination namespace; add native alias/held-lock regression. This extends existing finding 2, rather than representing newly introduced behavior.

**10. P2 — `416` completion ignores explicit representation changes, macOS & Windows.**  
`206` compares ETag & Last-Modified at [download.rs:449](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:449); `416` checks only URL & total at [download.rs:487](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:487).

Concrete sequence: chunked `200`, ETag `"v1"`, bytes `AAAA`, connection closes before terminating chunk; reconnect receives `416`, `Content-Range: bytes */4`, ETag `"v2"`. Saved total is unknown, so code publishes stale `AAAA` despite explicit contradiction. This requires server mishandling `If-Range`, precisely where adversarial validation matters: [RFC 9110 §13.1.5](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.5) requires matching validators or ignoring Range.

**Fix:** one representation-validation function for both statuses; reject/restart every contradictory validator before completion. Require trusted hash or affirmative current-representation evidence for `416`; test chunked/unknown-total cases.

**11. P2 — Scoped bypass fix can disclose previously direct traffic to proxy, Windows.**  
[system_proxy.rs:121](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:121) drops `http://private.example`; [system_proxy.rs:141](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:141) still returns `Single`. Requests now traverse proxy, including plaintext Authorization/body. This is newly reversed routing, **not fail-closed**. Microsoft documents [scheme-scoped bypass rules & plaintext HTTP proxy visibility](https://learn.microsoft.com/en-us/deployedge/configure-microsoft-edge-proxy-support).

**Fix:** return typed configuration error for unsupported scoped bypass entries before either client can send. Routing implementation can remain deferred. Add HTTP & HTTPS cases asserting zero origin/proxy requests on rejection.

**NON-BLOCKING**

**12. P3 — Claimed state-table coverage includes regressions that no longer reach their branches.**  
[third_read.rs:100](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/third_read.rs:100) supplies no ETag/hash. New prefix guard therefore restarts before Range; tests never receive changed-resource `416`, although [fourth_read.rs:8](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/fourth_read.rs:8) cites them as coverage. **Fix:** supply strong validator, assert Range & actual `416` response count, then assert restart. Make replacement-fixture failure at [download.rs:1367](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1367) fail qualification too.

DO NOT PUBLISH: 9, 10, 11
