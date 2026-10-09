# macOS codesign shim

`right-release build`, its release worker & `right-release dev` install a private,
executable `codesign` shell wrapper under
`<RIGHTKIT_MANAGED_ROOT>/signing/codesign-bin/<wrapper-hash>/` & prepend that
directory to child PATH. Existing Cargo shim ordering remains intact. Command
environment overrides receive the same injection. Defaults use
`/Volumes/D/.rightkit-managed`.

Session selection reuses `detectMacSigningSession`: SSH, non-Aqua & denied login
keychain access select the signing agent. Aqua selects immediate shell
`exec /usr/bin/codesign "$@"`, without launching Node or contacting the agent.
An SSH child also selects the agent when inheriting a GUI wrapper.

`codesign-argv` requests carry original argv, cwd, derived target paths & a bounded
deadline. The Aqua agent validates on admission & immediately before execution,
then invokes only `/usr/bin/codesign`, without a shell or caller environment.
Exit status, stdout & stderr are relayed without trimming or legacy output
truncation, including unsuccessful codesign exits. Transport/execution errors
fail explicitly; output exceeding the existing 1 MiB frame limit fails rather
than returning partial success. Jobs retain the existing 180-second deadline.

Accepted operations:

- Sign: `-s`/`--sign`, `--force`, `--options` (named flags including `runtime`),
  `--timestamp`/`--timestamp=none`, input `--entitlements`, `--deep`,
  `--preserve-metadata`, `--identifier` & verbosity. Ad-hoc `-s -` supports dev.
- Verify: `-v`/`--verify`, `--strict`, `--deep` & verbosity.
- Display: `-d`/`--display`, verbosity, `-r-`/`--requirements -` &
  `--entitlements :-`/`--entitlements -` for stdout.

Combined `-dvv`/`-vvv`, allowed long-option `=value` forms, relative paths,
multiple targets & `--` operands are supported. Keychain selection, detached
signatures, certificate extraction, file-producing display options, conflicting
operations & unknown flags are rejected. Every target, cwd & entitlement input
must remain lexically & canonically inside installed roots; native realpath
rejects symlink/`..` escapes. Caller roots may narrow installed policy only.

Installed roots include `/Volumes/D/claude`, managed storage, legacy broker &
release caches, `/tmp`/`/private/tmp`, installation TMPDIR & this uid's native
Darwin temp directory. Dedicated external Cargo targets can be configured with
repeatable `signing-agent install --cargo-target-dir <absolute-directory>`.

## Absolute paths & Tauri lookup

PATH shims cannot intercept tools invoking `/usr/bin/codesign` by absolute path.
Those callers must use PATH lookup or RightKit's signing API. Xcode callers
that use absolute paths require their own supported codesign-tool override.

Tauri CLI 2.11.4 delegates bundler signing through `tauri-macos-sign`:
[macOS bundler sign.rs](https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.11.4/crates/tauri-bundler/src/bundle/macos/sign.rs#L59)
calls `Keychain::sign`; [keychain.rs](https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.11.4/crates/tauri-macos-sign/src/keychain.rs#L212)
uses `Command::new("codesign")`. Its `/usr/bin/codesign` string at line 104 is
a certificate-import trusted-executable argument, not a signing invocation.
Source grep confirms both occurrences. MailRight's configured identity uses
the default keychain path; certificate-import mode's `--keychain` is rejected.

## MailRight proof from Windows through Mac

Refresh the resident agent once from this checkout so it understands the new
protocol & roots, then build/seal with this checkout's CLI. These commands
publish nothing:

```sh
win "ssh mac 'cd /Volumes/D/claude/mailright && node /Volumes/D/claude/rightkit/packages/release/cli/right-release.mjs signing-agent install'"
win "ssh mac 'cd /Volumes/D/claude/mailright && node /Volumes/D/claude/rightkit/packages/release/cli/right-release.mjs build --platform mac'"
win "ssh mac 'tail -n 100 ~/Library/Application\ Support/Orthic\ Labs/shared/signing-agent/requests.jsonl'"
```

Agent log must show fresh `request.accepted`, `request.started` &
`request.completed` events with matching job `id`, `type: "codesign-argv"` &
`status: 0`. This request type identifies shim traffic; older RightKit signing
helpers use `type: "codesign"`. Log contains metadata only, never argv, keys,
tokens or command output. Final build output must report its sealed release.
