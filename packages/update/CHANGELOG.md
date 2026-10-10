# Changelog

## 0.1.0

- New package `@rightkit/update` with the `right-update` CLI.
- `right-update check [--repo <path>] [--json]` compares every pinned dependency with its upstream latest for cargo (crates.io), pnpm (`pnpm outdated --json`, npm registry fallback), swiftpm (GitHub tags), rightkit (registry section of the versions file, plus workflow drift through `@rightkit/git`), and external tools (GitHub release or version URL, with per-platform asset checks).
- `.rightkit/update.json` manifest with a JSON Schema (`update.schema.json`) and a hand validator. Tool entries carry per-platform assets, checksums, optional archive `extract` member, and an object-shaped `journey`.
- Policy resolution: explicit entry policy, `policy.held` names always held, tier patterns (default `/sign|secur|licen|credential/`) held, then `policy.default`.
- Exit codes: 0 all current, 1 any outdated, 2 error.
- `apply`, `gate` and `land` are stubs that exit 2 with "not implemented".
