VERDICT: DO NOT PUBLISH — destination normalization introduces destructive overwrite widening, & inherited ACLs still permit exposure of downloaded private bytes.

Candidate citations reference `3c917803`; paths are relative to `crates/rightkit-http`. **B0:** published `14b7c74f` has [no downloader (`src/lib.rs:13`)](https://github.com/Orthic-Labs/rightkit/blob/14b7c74f/crates/rightkit-http/src/lib.rs#L13). At requested rule-1 anchor `fdad2f20`, [`send_any` returns buffered bytes without filesystem publication (`src/client.rs:305`)](https://github.com/Orthic-Labs/rightkit/blob/fdad2f20/crates/rightkit-http/src/client.rs#L305); this function is unchanged between those baseline commits.

| Earlier finding | Status & classification |
|---|---|
| Parent aliases / active-lock replacement | **Closed, N:** resolved ancestry checked before coordination creation. [download.rs:1083](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1083) |
| Concurrent corruption through that alias | **Closed, N:** layout & rename use resolved target. [download.rs:361](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:361), [436](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:436) |
| Unix `0600` → `0644` replacement | **Closed for mode bits, N; ACL exposure remains below.** [download.rs:793](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:793), [827](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:827) |
| Silently skipped replacement fixture | **Closed, N:** failed fixture rename panics. [download.rs:1872](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1872) |
| Contradictory `416` completion | **Closed by removal, N:** unconditional restart; wire sequence asserted. [download.rs:574](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:574), [fifth_read.rs:88](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/fifth_read.rs:88) |
| Scheme-scoped bypass disclosure | **Closed, N:** unsupported entries refuse configuration. [system_proxy.rs:160](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:160) |
| Unreachable regression coverage | **Partly closed, N:** valid replacement coverage exists; misleading older fixtures remain. [third_read.rs:123](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/third_read.rs:123) |
| System-proxy `.ok()` fallback | **Still open, N; non-blocking.** [client.rs:327](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:327) |
| Undocumented `Response` construction break | **Closed by documentation, N.** [CHANGELOG.md:7](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:7) |

**Blockers**

1. **N — directory-style destinations become destructive file replacements.**
   - **Baseline:** B0; no destination replacement.
   - **Candidate:** [`resolve_destination`:1073](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1073) rebuilds destination from `parent()` & `file_name()`, then [renames onto it:436](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:436). Rust explicitly normalizes `foo.txt/.` & trailing separators when extracting [`file_name()`]( /Users/adrdsouza/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/path.rs:2691).
   - **Reachable consumer:** Unix `download_to_path(url, Path::new("/data/important.bin/."), defaults)` where `important.bin` is an existing regular file. Candidate overwrites `/data/important.bin`. First candidate passed original pathname to [`rename`, 0f0395ee:401](https://github.com/Orthic-Labs/rightkit/blob/0f0395ee/crates/rightkit-http/src/download.rs#L401), which fails because that prefix is not a directory. [Native contract]( /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/share/man/man2/rename.2:321).
   - **Invariant:** resolution must preserve caller pathname’s directory requirements; normalization must not turn refusal into data loss.
   - **Smallest clearance:** reject raw destinations ending in separators or terminal `/.` before component normalization; test existing-file preservation.

2. **N — ACL inheritance exposes private downloads; mode-only protection is insufficient.**
   - **Baseline:** B0; no staging or replacement exposure.
   - **Candidate:** [directory creation:903](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:903) & [partial creation:793](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:793) inherit ACLs; [`restrict_part`:811](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:811) changes Unix mode bits only. Publication preserves no destination ACL.
   - **Reachable consumer:** call `download_to_path` for confidential content under macOS parent granting inheritable group read/search rights, replacing an existing `0600` file whose ACL excludes that group—or creating a fresh destination. Staging inherits those grants despite `0600`; group access survives publication. This follows [Apple’s ACL precedence & inheritance rules](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/FileSystemDetails/FileSystemDetails.html). Windows likewise inherits parent access instead of preserving a private destination’s DACL, as [README:101](/Volumes/D/claude/rightkit/crates/rightkit-http/README.md:101) discloses & [Microsoft documents](https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights).
   - **Invariant:** downloading private bytes must not broaden readership during staging or publication.
   - **Smallest clearance:** establish private staging ACLs before receiving bytes & refuse unsafe resumed partials; alternatively defer affected download paths. **N** reflects publication absent from baseline, not merely an incomplete guard.

**Notes**

- Stable symlink ancestry, `..` & case/Kelvin spellings are covered by canonicalization plus [component folding:1048](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1048). Parent swaps remain explicitly disclosed in [README:100](/Volumes/D/claude/rightkit/crates/rightkit-http/README.md:100).
- Windows canonicalization follows junctions; native rename accepts its verbatim output. Checked leaf remains unchanged, so verbatim conversion itself does not bypass [reserved-name checks:1104](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:1104). [Windows canonicalize implementation]( /Users/adrdsouza/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/sys/fs/windows.rs:1593).
- **N, recoverability note; baseline B0:** failed rename attempts handle-based `0600` restoration, but [discards restoration errors:441](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:441). Successful restoration permits reopening; unconditional usable-partial preservation is not established.
- **N, routing note:** baseline has [no System configuration](https://github.com/Orthic-Labs/rightkit/blob/fdad2f20/crates/rightkit-http/src/client.rs#L52). `Client::try_new` with `System` can [fall back to environment:305](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:305) after proxy construction fails, violating exact-or-refuse routing; irreversible harm remains unestablished.

**Silent API breaks & untested promises**

No additional silent source break found: `Response`, `Request`, configuration & error-shape changes are documented at [CHANGELOG:7](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:7), [281](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:281), [294](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:294).

New [publication tests:92](/Volumes/D/claude/rightkit/crates/rightkit-http/tests/download_publication.rs:92) cover Unix aliases, mode preservation, fresh & resumed partials. Missing: Windows parent-junction/verbatim publication, ACL readership, directory-style destinations & rename-failure restoration—the latter acknowledged at [CHANGELOG:6](/Volumes/D/claude/rightkit/crates/rightkit-http/CHANGELOG.md:6). System zero-request coverage remains [helper-level:1918](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:1918).

**What could not be established**

Runtime reproduction, universal filesystem-alias exclusion, registry identity & independent CI verification were not established. Supplied CI was context. Review remained read-only; no builds, tests or edits performed.
