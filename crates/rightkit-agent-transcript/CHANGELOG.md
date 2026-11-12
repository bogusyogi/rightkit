# Changelog

## 0.1.3

- Absorbed-stream rows of any agent server's host, not only CodeRight's: `rightkit-agent-server`'s exporter names its rows after the app it runs in (`<app>_event_v1`, `host: "<app>"`), and 0.1.2 dropped every row that was not `coderight_event_v1` from `coderight`. New `parse_absorbed_rows_for_host(host, session_id, tip_digest, rows)` normalizes a session's rows for its own host (every event carries that host), `adapters::host_events(host, row)` reads one such row and `adapters::host_event_type(host)` names its type; `iter_events_for_host` reads a `<host>_event_v1` row of the caller's host. A row of another host, or whose type and `host` disagree, still yields nothing. Additive: `parse_absorbed_rows` and `coderight_events` are unchanged (they are the host `coderight`).

## 0.1.2

- Manifest only: dependency requirements and workspace metadata changed since 0.1.1 was published; no source change.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `membrane-transcript` (engine).
