# rightkit-sandbox 0.5.1 registry consumer (PK12 sandbox→browser edge)

Standalone consumer, outside the workspace, pinning `rightkit-sandbox =0.5.1`
exactly with default features. No path, Git, workspace, patch, or test-seam
dependency, and no `Cargo.lock` committed in this directory (the validator
rejects it); the harness pins and retains lock output outside the recipe.

Runs on Windows and macOS. Other hosts exercise the typed `Unsupported`
path and exit nonzero. No runner assets, no CFT bundle, no network, no
real user data. State is one temp directory tree, removed on drop.

## What it proves

| Proof row | Executed behavior |
|---|---|
| `reserve_before_spawn` | `Strict` refuses `ChildProcess` with typed `ResourceDenied` (`resource_admission_denied`); `Practical` reserves, releases on drop, and reserves again; `Strict` `Sandbox::spawn` returns `ResourceDenied` without creating a process. |
| `pure_planning` | `glob_matches` anchored semantics (`*` stays within one segment); `path_is_within` containment; `plan_deny_read_paths` refuses a relative path; `classify_denial` reports `Filesystem` for `deny file-read` and `Unrecognized` for unrelated output. |
| `confined_echo` | `Sandbox::spawn` with `allow_read(ro)` + `allow_write(rw)` runs the platform shell (`cmd.exe /c` on Windows, `/bin/sh -c` elsewhere) with cwd inside `rw`; the child writes `allowed.txt` there, exits successfully with no denial and no timeout; the parent reads the bytes back and `ro` gains no file. |
| `sandbox_r2` | Emitted only after every preceding row passes. |

## Not covered

The staged 0.5.1 Windows pipe-pair `BestEffort` ancestor leases and the
verbatim-prefix deny-glob fix need real-user stores and stay proved by the
package gate rows, not this recipe. Also not exercised: `spawn_piped`,
`spawn_in_profile`, `spawn_with_pipe_pair`, PTY sessions, `confine_command`,
loopback/AppContainer profiles, network policy, `report_denial` grant
planning, and `secret_paths_within` against real secret folders. This recipe
adds no feature, version, or public API and runs no commands, builds, or
tests while being prepared.
