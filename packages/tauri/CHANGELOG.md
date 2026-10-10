# Changelog

## 0.2.0 — 2026-10-09

Breaking:
- `@rightkit/updates` moved from `dependencies` (pinned `0.2.4`) to `peerDependencies` (`^0.2.5`). Apps must install `@rightkit/updates` themselves; it is used only as the `AutoUpdateClient` type (`ca41ba38`).

Changed:
- `@rightkit/logs` `^0.1.4` is now a dependency. The `rightkit_logs_write`, `rightkit_logs_collect` and `rightkit_logs_clear` command names come from its `RIGHTKIT_LOGS_WRITE_COMMAND`, `RIGHTKIT_LOGS_COLLECT_COMMAND` and `RIGHTKIT_LOGS_CLEAR_COMMAND` constants instead of string literals. The command strings are unchanged (`ca41ba38`).
- Public types are unchanged (`ca41ba38`).

## 0.1.1 — 2026-08-28

- Dependency bump only: `@rightkit/updates` `workspace:0.2.3` to `workspace:0.2.4` (`343654bf`).

## 0.1.0 — 2026-07-14

- First version under the name `@rightkit/tauri`: typed host adapters for shared Right Suite Tauri commands (`1cb6b4c4`). Commits through `64de23bf` landed on this version without a version bump.
