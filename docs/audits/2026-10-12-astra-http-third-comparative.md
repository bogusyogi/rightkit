VERDICT: DO NOT PUBLISH — download publication still exposes private bytes, & newly added create/reopen gap permits deletion of another inode.

Candidate: `bdf35c60`. Paths below reference current source. **B0**, used by every download finding: published `14b7c74f` has [no downloader (`lib.rs:13`)](https://github.com/bogusyogi/rightkit/blob/14b7c74f/crates/rightkit-http/src/lib.rs#L13); requested anchor [`fdad2f20`, `client.rs:305`](https://github.com/bogusyogi/rightkit/blob/fdad2f20/crates/rightkit-http/src/client.rs#L305) returns buffered bytes without filesystem publication. Thus **N** classifies added harmful publication, not merely incomplete guards.

| Earlier finding | Status & classification |
|---|---|
| Parent aliases / active-lock replacement | Closed, N: [resolved ancestry checked:1194](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1194). |
| Concurrent corruption through those aliases | Closed, N: [resolved target used:416](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:416). |
| Unix `0600` → `0644` | Closed for ordinary mode bits, N; [mode copying:888](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:888) still mishandles ACL protection below. |
| Raw directory suffix overwrite | Closed, N: [raw check:1094](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1094), [preservation test:161](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/download_publication.rs:161). |
| Inherited ACL exposure | Partly closed, underlying N remains: blockers 1–2. |
| Skipped replacement fixture | Closed, N: [rename failure panics:2038](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:2038). |
| Contradictory `416` completion | Closed by removal, N: [restart:624](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:624), [wire assertions:88](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/fifth_read.rs:88). |
| Scheme-scoped bypass disclosure | Closed, N: [typed refusal:160](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:160). |
| Unreachable regression coverage | Partly closed, N: older [fixture:123](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/third_read.rs:123) lacks validator; [restart:521](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:521) prevents its claimed ranged branch. |
| System `.ok()` fallback | Still open, N; note below. |
| Undocumented `Response` construction break | Closed by [documentation:10](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:10), N. |

**Blockers**

1. **N — Windows resumed partials retain readable DACLs.**  
   **Baseline:** B0. **Candidate:** [resume opens existing inode:784](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:784); [privacy check:815](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:815) calls Windows’ [unconditional `true`:87](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/private.rs:87). **Reachable:** `download_to_path(..., resume: true)` with matching claim, expected hash & leftover partial carrying protected explicit Everyone-read permission. Hardening existing `.rk-download` [replaces its DACL:430](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/private.rs:430), but [protected children resist inheritance](https://learn.microsoft.com/en-us/windows/win32/secauthz/ace-inheritance); directory access does not generally gate Windows child access, & [rename preserves file security](https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights). Downloaded bytes remain readable during staging & after [publication:486](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:486). **Invariant:** private downloads must not acquire additional readers. **Smallest fix:** refuse existing Windows partials unless verified private before use; recreate unsafe partials through protected creation. [Disclosure:101](/Volumes/D/claude/rightkit/crates/rightkit-http/README.md:101) does not clear an N.

2. **N — Unix publication copies modes, not effective access restrictions.**  
   **Baseline:** B0. **Candidate:** [copies `mode & 0777`:891](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:891) before rename. **Reachable:** ordinary `download_to_path` replacing macOS `0644` file with explicit Bob-deny-read ACL, under ACL-free parent: replacement loses denial, exposing bytes to Bob ([Apple ACL precedence](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/FileSystemDetails/FileSystemDetails.html)). Linux likewise inherits named-user entries under [mode-only creation:197](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/private.rs:197); replacing ACL-free `0640` destination raises staged ACL mask, activating inherited Bob-read entry ([ACL permission mapping](https://manpages.debian.org/unstable/acl/acl.5.en.html)). **Invariant:** replacement must not broaden readership. **Smallest fix:** retain private `0600` publication, or refuse affected ACL cases until effective permissions can be preserved.

3. **N — fresh partial reopen loses creation identity.**  
   **Baseline:** B0. First candidate [retained exclusive-create handle:744](https://github.com/bogusyogi/rightkit/blob/0f0395ee/crates/rightkit-http/src/download.rs#L744). **Candidate:** [drops it & reopens pathname:859](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:859). **Reachable:** Unix `download_to_path(..., resume: false)` while another same-user process renames nonempty, single-linked file over empty partial during this gap. [Regular-file/link checks:792](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:792) accept replacement; downloaded bytes append, then [length mismatch deletes it:446](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:446). First candidate retained original inode & detected replacement before publication. **Invariant:** fresh download must write exclusively created inode. **Smallest fix:** obtain read/write private creation handle directly; retain it throughout.

**Notes**

- Closing newly created partial does **not** release separate [claim lock:1029](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1029). Reopening another inode can affect that inode’s POSIX locks upon close; [filesystem helper explains distinction:140](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/acl.rs:140).
- Raw [separator checks:1138](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1138) & [terminal dot/space refusal:1216](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1216) cover requested suffix spellings. Ordinary existing directory destinations still reach final rename refusal.
- N routing note: baseline [configuration:52](https://github.com/bogusyogi/rightkit/blob/fdad2f20/crates/rightkit-http/src/client.rs#L52) lacks System mode. `Client::try_new(System)` can [discard proxy-construction failure:327](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:327) & select environment routing, violating exact-or-refuse intent; irreversible harm not established.

**Silent API breaks & untested promises**

No additional silent source break found. Required `rightkit-fs/fs`, including no-default-feature consumers, is deliberate & [documented:6](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:6); [feature definition:147](/Volumes/D/claude/rightkit/crates/rightkit-fs/Cargo.toml:147) adds `thiserror` & Unix `libc`, without enabling `lock`, `json` or `digest`.

[ACL tests:107](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/download_acl.rs:107) cover macOS refusal & Windows fresh publication, not blockers above. No dedicated Windows UNC/parent-junction preservation, reopen-race, or rename-restoration test found; [restoration gap is acknowledged:9](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:9).

**What could not be established**

`SetNamedSecurityInfoW` acceptance of passed [verbatim pathname:429](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/private.rs:429) is not established; [Microsoft’s documented formats](https://learn.microsoft.com/en-us/windows/win32/api/accctrl/ne-accctrl-se_object_type) do not explicitly settle it. Compilation, runtime reproduction & candidate CI results remain unestablished; supplied earlier CI is context only.

Read-only review completed; no builds, tests or modifications.
