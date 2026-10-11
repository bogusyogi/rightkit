1. **P1 — Resume still corrupts hard-linked files.** [download.rs:553](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:553), [download.rs:375](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:375). On macOS or Windows, create `out.part` as a hard link to `precious.txt`, then download with `resume=true`. Regular-file & no-follow checks accept it; HTTP `200` truncates & overwrites both names. Final identity verification also passes. **Smallest fix:** reject multiply linked opened partials before mutation, using Unix `nlink` / Windows link count; preserve victim contents in regression assertions.

2. **P1 — Another download can replace an active lock.** [download.rs:625](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:625), [download.rs:266](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:266). On macOS: download A holds `out.lock` & pauses after writing 50 bytes; download B targets `out.lock`, replacing its directory entry through rename. Download C targeting `out` locks B’s new inode, resumes, receives `200`, truncates & publishes 100 bytes. A subsequently appends its remaining 50 bytes through its still-open handle, corrupting C’s published file. Length verification happens too late. **Smallest fix:** reserve `.lock` & `.part` destination suffixes, rejecting them before filesystem access with platform-appropriate case handling; alternatively isolate internal files in a reserved directory.

3. **P2 — `416` bypasses resource-change protection.** [download.rs:355](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:355), [download.rs:389](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:389), [download.rs:477](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:477). First response from `/a` advertises 10 bytes, sends `AAAA`, then disconnects. Reconnect redirects to four-byte `/b`, which returns `416; Content-Range: bytes */4`. Code publishes `AAAA` without fetching `/b`: source comparison exists only inside `206`. Query stripping also conflates `/download?id=A` & `?id=B`. **Smallest fix:** compare complete resource identity, including query, before accepting either `206` or `416`; restart on change. Existing [resource-change test](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/audit_fixes.rs:401) covers only changed paths returning `206`.

4. **P2 — OAuth provider errors expose submitted credentials.** [oauth/lib.rs:527](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/lib.rs:527), [http/client.rs:561](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:561), [oauth/lib.rs:601](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/lib.rs:601). Form fields never register redaction values. Refresh with token `single-use`; return HTTP `400` JSON `{"error":"invalid_request","error_description":"rejected single-use"}`. `OAuthError` retains & prints that token. HTTP `200` provider errors bypass HTTP-status scrubbing entirely. **Smallest fix:** sanitize provider error fields against submitted secrets before constructing `OAuthError`, covering both status paths & device polling.

5. **P3 — Windows safety tests can report success without exercising protection.** [second_read.rs:70](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/second_read.rs:70), [download.rs:1011](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1011). Without symlink privileges, both tests return successfully before assertions. [Junction test](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/second_read.rs:90) also passes with no-follow protection removed: pre-existing directory/junction rejection suffices. **Smallest fix:** require fixture creation on Windows qualification runners & retain mandatory check/open swap assertions.

**Previous second-read findings**

| # | Disposition | Current source & test evidence |
|---|---|---|
| 1 | **FIXED** | Windows reparse-point refusal before mutation: [download.rs:664](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:664); swap assertion at line 1021. Hard links remain separate finding 1 above. |
| 2 | **PARTLY FIXED** | Lock acquired before opening partial at [download.rs:202](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:202); actual length checked at line 226. [Concurrent-call test](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/second_read.rs:128) covers ordinary contention, missing finding 2 above. |
| 3 | **FIXED** | [wire.rs:93](/Volumes/D/claude/rightkit/crates/rightkit-http/src/wire.rs:93) handles leading blank lines; [download.rs:489](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:489) rejects unobserved heads. Negative & positive controls: [second_read.rs:252](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/second_read.rs:252), line 302. |
| 4 | **FIXED** | Pending streams register cancellation timer at [async_client.rs:788](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:788); [silent-server test](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/second_read_async.rs:41) requires cancellation & termination. |
| 5 | **FIXED** | Post-drain sleep clamped at [client.rs:1125](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:1125); [timing regression](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/second_read.rs:322) distinguishes old overrun. |
| 6 | **FIXED** | [oauth/e2e.rs:283](/Volumes/D/claude/rightkit/crates/rightkit-oauth/tests/e2e.rs:283) uses loopback policy; lines 307–312 require provider `503` & exactly one request. |

**Inherited first-read dispositions:** all eight remain **FIXED**, including previously partial #6:

| # | Current implementation evidence |
|---|---|
| 1 | HTTP/1.1 ALPN: [async_client.rs:114](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:114). |
| 2 | Explicit Mozilla roots: [async_client.rs:102](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:102). |
| 3 | Shared proxy routing decision: [async_client.rs:596](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:596). |
| 4 | Whole-resource completion: [download.rs:241](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:241). |
| 5 | Async drain budgets: [async_client.rs:381](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:381). |
| 6 | Fail-closed encoding observation: [download.rs:489](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:489). |
| 7 | Checked exclusive range end: [download.rs:848](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:848). |
| 8 | Response debug redaction: [client.rs:700](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:700), [async_client.rs:249](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:249). |

Source-only review at main `48f8fa01`; no builds, tests, or project execution.

**DO NOT PUBLISH — blocking findings 1–4.**