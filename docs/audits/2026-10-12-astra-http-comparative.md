VERDICT: DO NOT PUBLISH — newly added download publication permits silent corruption through parent aliases & widening of private-file permissions.

Paths below are relative to `crates/rightkit-http`; candidate citations refer to `0f0395ee`. **B0:** published baseline has no downloader ([14b7c74f, src/lib.rs:13](https://github.com/Orthic-Labs/rightkit/blob/14b7c74f/crates/rightkit-http/src/lib.rs#L13)); `send_any` returns memory buffers without publishing files ([src/client.rs:305](https://github.com/Orthic-Labs/rightkit/blob/14b7c74f/crates/rightkit-http/src/client.rs#L305)).

**Earlier open findings**

| Finding | Status & classification |
|---|---|
| Fourth 2 / fifth 9 — active-lock aliases | Original Kelvin-sign destination case closed by relocation; broader exclusion remains **N**, blocker 1. [src/download.rs:782](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:782) |
| Second 2 — concurrent corruption | Still open, **N**, through blocker 1. |
| Fourth 5 — silently skipped replacement fixture | Closed: failed rename now panics. [src/download.rs:1748](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1748) |
| Fourth 6 / fifth 10 — contradictory `416` completion | Closed by removal: every ranged `416` restarts. Actual Range/If-Range sequence asserted. [src/download.rs:531](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:531), [tests/fifth_read.rs:88](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/fifth_read.rs:88) |
| Fourth 7 / fifth 11 — scheme-scoped bypass disclosure | Closed: unsupported bypass produces typed refusal before sending. [src/system_proxy.rs:160](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:160), [src/client.rs:1351](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:1351) |
| Fifth 12 — unreachable regression coverage | Partly closed by new `416` test; stale coverage claim remains **N**, test-only. See notes. |
| Fifth class-level `.ok()` fallback | Still open, **N**, non-blocking configuration note below. [src/client.rs:327](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:327) |

**Blockers**

1. **N — parent symlink bypass lets concurrent downloads corrupt already-published bytes.**
   - **Baseline:** B0; no coordination files or download publication.
   - **Candidate:** parent exclusion compares lexical components only ([src/download.rs:952](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:952)); layout & rename subsequently follow parent paths ([src/download.rs:782](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:782), [401](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:401)).
   - **Reachable path:** on Unix, let `alias` symlink to `dir/.rk-download`. While download A targets `dir/out.bin`, call `download_to_path` for `alias/<SHA256(out.bin)>.lock`, serving literal `out.bin`. This replaces A’s held claim inode. Download C targeting `dir/out.bin` with `resume: true` now acquires replacement claim, accepts its record, & opens A’s partial inode ([src/download.rs:883](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:883), [909](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:909), [689](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:689)). C can truncate, finish, verify & rename that inode successfully; A’s next append then changes C’s published destination ([src/download.rs:428](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:428), [577](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:577)).
   - **Invariant:** successful publication must remain isolated from other download writers.
   - **Smallest targeted clearance:** reject resolved parent ancestry inside coordination directories, retaining directory identity through publication; add this held-claim alias regression.

2. **N — Unix publication broadens existing private-file permissions.**
   - **Baseline:** B0; HTTP calls do not replace destination inodes.
   - **Candidate:** partial creation sets no Unix mode ([src/download.rs:728](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:728), [744](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:744)); successful rename replaces destination without preserving its permissions ([src/download.rs:388](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:388)).
   - **Reachable path:** call `download_to_path` for an existing `0600` secret file inside a traversable `0755` directory, under umask `022`. Partial creation uses `0666 & ~umask`, hence `0644`; rename exposes downloaded private bytes. Rust documents that creation default at [std/os/unix/fs.rs:477](/Users/adrdsouza/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/os/unix/fs.rs:477). Owner-only staging directory protection ends upon publication.
   - **Invariant:** replacing private content must not broaden readership.
   - **Smallest targeted clearance:** create Unix partials owner-only & enforce equivalent protection on resumed files before publication.

**Notes — non-blocking**

- **N, coverage claim:** B0 has no download tests. Candidate’s changed-resource fixtures supply neither ETag nor hash, so `download_to_path` discards their prefix before requesting Range; they cannot exercise claimed `416` branch. Invariant: named regression tests reach named behavior. [tests/third_read.rs:123](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/third_read.rs:123), [src/download.rs:428](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:428), [tests/fourth_read.rs:9](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/fourth_read.rs:9).
- **N, configuration:** baseline has no System mode ([14b7c74f, src/client.rs:55](https://github.com/Orthic-Labs/rightkit/blob/14b7c74f/crates/rightkit-http/src/client.rs#L55)). `Client::try_new(config.proxy(ProxyMode::System))` still converts failed static-proxy construction into environment fallback, contradicting exact-or-refuse routing. Irreversible harm from this case is not established. [src/client.rs:327](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:327), [305](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:305).

**Silent API breaks & untested promises**

- **N, source compatibility:** baseline permits external `Response { status, headers, body, attempts }` followed by `Response::json`; candidate’s private `redact` field prevents construction. Changelog documents private `Request` fields but omits this `Response` break. [baseline src/client.rs:212](https://github.com/Orthic-Labs/rightkit/blob/14b7c74f/crates/rightkit-http/src/client.rs#L212), [candidate src/client.rs:741](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:741), [CHANGELOG.md:282](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:282).
- Other principal breaks are documented: enum exhaustiveness, trust defaults, error shapes & configuration fields. [CHANGELOG.md:35](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:35), [51](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:51), [269](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:269).
- README isolation promise lacks parent-alias coverage; tests enumerate lexical spellings only. [README.md:93](/Volumes/D/claude/rightkit/crates/rightkit-http/README.md:93), [tests/fifth_read.rs:239](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/fifth_read.rs:239).
- System zero-request promise has parser/helper assertions, but no end-to-end origin/proxy request-count test found. [README.md:167](/Volumes/D/claude/rightkit/crates/rightkit-http/README.md:167), [src/client.rs:1918](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:1918).

**What could not be established**

Runtime reproduction, registry-archive identity, & independent CI-result verification were not established. Supplied CI results were accepted as context. Review used read-only inspection; no builds, tests, or edits performed.
