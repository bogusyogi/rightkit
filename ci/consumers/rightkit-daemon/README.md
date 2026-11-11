# rightkit-daemon 0.8.0 registry consumer (PK07-3 + PK07 R2)

Standalone consumer, outside the workspace, pinning `rightkit-daemon =0.8.0` exactly with default
features only. It exercises the root-cutover surface a registered app uses to move its data root:
`Suite::per_run_temp`, `Suite::app_root`, `RootCutover` with a `FsMove::Tree` copy and one no-op
`CutoverParticipant`, `status`, `selected_root`, the completed rerun, `recover`, and
`retire_old_root`. It also checks that a relative old root is refused with `InvalidRoot`. Since 0.8.0 these
root items live in `rightkit-roots` 0.1.0 and are re-exported here under their old paths, so this recipe
proves the re-exports a not-yet-migrated app compiles against.

It also runs frozen PK07 R2 example: `Service::builder().methods(..).handler(..).local(..).start()`
with `Client::connect_expecting` against this process, echo round-trip, executable-mismatch refusal,
unlisted-app refusal & `ERROR_RATE_LIMITED` pin. Extra dependency: `serde_json = "1"`.

State: per-run private temp suites, removed on drop. Local current-user transport only; no network/TCP,
no real user data. The `remote-loopback` feature is not enabled (`remote`, `link` and `relay` are `rightkit-daemon-remote` since 0.7.0).

Not covered: `FsMove::Companions`, participant failure, abort and panic paths, path substitution,
the db relocation participant, approvals (PK07-4), separate-process host, takeover across processes,
and the PK06 client lifecycle. This recipe must be revisited if the PK07 public API changes.

Run only by the CI consumer lane.
