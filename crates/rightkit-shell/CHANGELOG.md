# Changelog

All notable changes to `rightkit-shell`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.2.0

- Breaking: the `engine` feature (engine sidecar wiring over `rightkit-jobs` and
  `rightkit-process`) is no longer a default feature. It pulled `rightkit-jobs`
  and, through it, `db`, `time`, and `uuid` into every consumer. Apps that use
  `Shell::builder(..).engine(..)`, `shell::engine`, or `Shell::engine` now enable
  it: `features = ["engine"]`. `default` is now `["wv2-0_38"]`.
- The example app opts in to `engine` explicitly, and drops its unused
  `rightkit-service` dev-dependency and duplicate `rightkit-control` dev entry.
