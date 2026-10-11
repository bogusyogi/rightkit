# Astra read of rightkit-http 0.3.0 (working tree), 2026-10-11

Read-only review before publication. Verdict: do not publish. Findings verbatim:

Read-only review completed against HEAD, including dependency source. **Eight findings:**

1. **P1 — Default async HTTPS can panic on HTTP/2 negotiation.** [async_client.rs:74](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:74), [Cargo.toml:34](/Volumes/D/claude/rightkit/crates/rightkit-http/Cargo.toml:34).  
   Standalone consumer enables `async`; server presents valid certificate & selects advertised `h2`. Crate never enables reqwest’s `http2` feature, so hyper-util reaches `panic!("http2 feature is not enabled")`. Workspace feature unification can conceal this. Enable HTTP/2 explicitly or advertise only HTTP/1.1.

2. **P1 — Async `Bundled` trust can silently accept OS-installed roots.** [async_client.rs:98](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:98).  
   Another dependency enables reqwest `default-tls` or `rustls-tls-native-roots`; caller selects `TrustRoots::Bundled`; interception server presents certificate signed only by locally installed CA. Async accepts it while blocking rejects it, despite `verifier_kind()` reporting `"bundled"`. Bundled branch leaves backend/root selection to additive Cargo features. Install explicit Mozilla-only TLS configuration. [Reqwest documents this backend-selection hazard](https://docs.rs/reqwest/0.12.28/reqwest/tls/index.html).

3. **P1 — Async `NO_PROXY` disagreement bypasses promised direct-address vetting.** [async_client.rs:108](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:108), [async_client.rs:472](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:472).  
   Set environment proxy & `NO_PROXY=*.example.com`; request `http://api.example.com/`, whose proxy-side DNS resolves to `169.254.169.254`. Ureq’s matcher says direct, so precheck defers hostname vetting to DNS. Reqwest does **not** recognize that wildcard, routes through proxy & never invokes destination resolver. Metadata request reaches proxy without address vetting. Same mismatch applies after redirects. Use identical routing decisions for policy & transport.

4. **P2 — Short resumed range is accepted as complete download.** [download.rs:207](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:207).  
   Existing `.part` contains 80 bytes. Server answers `Range: bytes=80-` with `206`, `Content-Range: bytes 80-99/200`, `Content-Length: 20` & 20 bytes. `st.limit.or(st.total)` checks 100 instead of advertised total 200; without optional expected length/hash, truncated file is renamed into place successfully. Validate whole-resource completion separately from response-range length.

5. **P2 — Retry draining ignores async request/body deadlines.** [async_client.rs:313](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:313).  
   GET has 100 ms deadline, multiple attempts & zero backoff. Server immediately returns `503`, `Content-Length: 1`, then withholds body. Drain uses independent five-second deadline, ignoring request deadline, attempt timeout & `recv_body_timeout`; call takes roughly five seconds. Subsequent retry sleep also lacks post-drain deadline enforcement. Clamp draining & sleeping to remaining budgets.

6. **P2 — Chunked gzip bypasses download decompression refusal.** [download.rs:455](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:455).  
   Server ignores `Accept-Encoding: identity`, returns `200` with `Content-Encoding: gzip`, `Transfer-Encoding: chunked` & valid gzip payload. Ureq removes encoding/length headers but retains transfer encoding. Guard consequently accepts decoded body; absent expected size/hash, download succeeds with decoded bytes despite stated refusal, undermining encoded-byte resume accounting. Disable transparent decoding for downloads instead of inferring it from framing.

7. **P2 — Hostile `Content-Range` causes integer overflow.** [download.rs:334](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:334).  
   Resume one-byte `.part`; server returns chunked `206` with `Content-Range: bytes 1-18446744073709551615/*`. Parser accepts it; `end + 1` panics with overflow checks enabled, otherwise wraps to zero. Reject unrepresentable exclusive range ends using checked arithmetic.

8. **P2 — Response debug output loses configured secret redaction.** [client.rs:656](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:656), [client.rs:1290](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:1290).  
   Configure `redact_values(["SECRET"])` or send `.secret_header("X-Credential", "SECRET")`; server returns `X-Echo: SECRET`. Formatting response with `{:?}` prints secret because response debug retains neither configured nor per-request redaction values. `ResponseHead` shares this defect.

Direct connections use vetted resolver results; I found no direct DNS relookup gap, explicit certificate-verification disable switch, `https_only` bypass, or default POST/PATCH status replay. Proxied hostname destinations already have explicitly documented weaker SSRF protection.

Reading cannot establish actual OS trust-store behavior, certificate rejection across platforms, consumer feature combinations, or runtime timing. Existing TLS tests exercise plaintext peers, not valid/untrusted/expired/wrong-host certificate chains. No builds, tests, network reproductions or edits performed.

**Verdict: do not publish.**
