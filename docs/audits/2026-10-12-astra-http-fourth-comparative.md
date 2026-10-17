VERDICT: DO NOT PUBLISH — resumed inodes can expose newly downloaded bytes through existing readers, & pathname substitution can publish unverified bytes.

Candidate citations refer to `1f153b53`. **B0:** published `14b7c74f` has [no downloader (`lib.rs:13`)](https://github.com/bogusyogi/rightkit/blob/14b7c74f/crates/rightkit-http/src/lib.rs#L13); requested anchor [`fdad2f20`, `client.rs:305`](https://github.com/bogusyogi/rightkit/blob/fdad2f20/crates/rightkit-http/src/client.rs#L305) returns buffered bytes without filesystem mutation. Both blockers are **N versus B0**, retained from earlier candidates; incomplete hardening is **I**, not independently N.

| Earlier finding | Status & classification |
|---|---|
| Parent aliases / concurrent corruption through those aliases | Closed, N: [resolved-parent rejection:1158](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1158). |
| Mode widening / copied-mode ACL exposure | Closed by removal, N: [publication:481](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:481). |
| Directory-style destructive destinations | Closed, N: [raw refusal:1058](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1058). |
| Fresh inherited ACL exposure | Closed in ordinary creation path, N: [private creation:852](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:852). |
| Windows resumed-partial exposure | I hardening; underlying N remains: blocker 1. |
| Fresh create/reopen substitution | Specific gap closed, N: [identity comparison:855](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:855). Later windows remain below. |
| Failed-rename permission restoration | Closed by removal, N: [rename:484](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:484). |
| Skipped replacement fixture | Closed, N: [failure panics:2063](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:2063). |
| Contradictory `416` | Closed by removal, N: [restart:614](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:614). |
| Scheme-scoped bypass disclosure | Closed, N: [typed refusal:160](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:160). |
| Misleading older regression coverage | Still open, N, test-only: [fixture:123](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/third_read.rs:123) lacks validator; [prefix restart:511](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:511) bypasses claimed ranged branch. |
| System proxy fallback | Still open, N; note below. |
| Undocumented `Response` construction break | Closed by documentation, N: [CHANGELOG:11](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:11). |

**Blockers**

1. **N — resumed downloads reuse previously exposed inodes.**

   **Baseline:** B0 performs no file append/publication. **Candidate:** opens existing partial, changes permissions, then appends through retained handle ([download.rs:774](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:774), [802](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:802), [660](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:660)).

   **Reachable consumer:** normal `download_to_path(..., resume: true, expected_sha256: Some(...))` resumes matching leftover carrying Everyone-read on Windows, or previously readable Unix mode. Another principal already holds readable handle. DACL replacement/chmod does not revoke that handle; newly appended confidential bytes remain readable, including after rename. Windows explicitly [retains previously granted handle access](https://devblogs.microsoft.com/oldnewthing/20200320-00/?p=103579); filesystem helper recognizes same Unix limitation ([acl.rs:8](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/acl.rs:8)).

   **Adversary:** existing read access suffices; no destination-directory write access required. **Invariant:** fresh private bytes must never enter previously exposed inode. **Smallest clearance:** refuse on-disk resume for this release; alternatively copy accepted prefix into freshly private inode before receiving bytes. Permission repair alone cannot establish privacy.

2. **N — publication & cleanup still act on substitutable names.**

   **Baseline:** B0 cannot overwrite/delete files. **Candidate:** verifies open inode, then separately renames pathname ([481–484, starting here](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:481)); mismatch cleanup deletes pathname without identity binding ([433](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:433)).

   **Reachable consumer:** ordinary `download_to_path`, even with expected hash. Same-user process replaces `<key>.part` after final identity comparison; rename publishes replacement while report describes verified original. Swapping before mismatch cleanup deletes replacement instead.

   **Adversary:** requires write access inside `.rk-download`; destination-directory write access is unnecessary if separately restricted. An actor already able to replace destination directly is weaker evidence.

   **Invariant:** only verified, owned inode may be published/deleted. **Smallest release-safe clearance:** defer downloader until publication/cleanup bind to owned inode. This survives first candidate’s [check/rename:400](https://github.com/bogusyogi/rightkit/blob/0f0395ee/crates/rightkit-http/src/download.rs#L400), rather than arising from latest identity guard.

**Notes**

- New path-based hardening can narrow substituted file after handle/link checks: [download.rs:790](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:790), [private.rs:28](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/private.rs:28). Subsequent appends retain original handle; pathname substitution alone does **not** redirect writes. This adds foreign-file permission mutation versus first candidate; irreversible harm from narrowing alone is not established.
- `same_open_file` correctly compares Unix device/inode & ordinary Windows volume/file identity ([1377](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1377)); hard links identify same inode, then [link-count check:790](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:790) rejects multiple names. Write-only handle is not prohibited by [API contract](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileinformationbyhandle). ReFS’s [64-bit identity is not guaranteed unique](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/ns-fileapi-by_handle_file_information): I, incomplete identity protection; concrete collision harm not established.
- **N routing note:** baseline [configuration:52](https://github.com/bogusyogi/rightkit/blob/fdad2f20/crates/rightkit-http/src/client.rs#L52) lacks System mode. `Client::try_new(System)` still [discards construction failure:327](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:327), violating exact-or-refuse routing; irreversible harm not established.

**API & tests**

No additional silent API break found; privacy policy, dependency change & source breaks are documented ([CHANGELOG:5](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:5), [285](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:285)). No production `download_to_path` caller found under `crates/`; models uses [its own `stream_any` downloader:365](/Volumes/D/claude/rightkit/crates/rightkit-models/src/download.rs:365). No repository consumer access break established.

Mode/ACL tests exist ([publication:203](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/download_publication.rs:203), [ACL:341](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/download_acl.rs:341)); retained-reader, hardening-substitution, final-rename race & Windows junction/UNC coverage remain absent. Creation-race gap is explicitly [acknowledged:5](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:5).

**Not established:** compilation/runtime correctness since `3c917803`, verbatim-path acceptance by [SetNamedSecurityInfoW](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-setnamedsecurityinfow), or independent CI verification. Supplied CI is context only.

Read-only review completed; no builds, tests, or modifications.
