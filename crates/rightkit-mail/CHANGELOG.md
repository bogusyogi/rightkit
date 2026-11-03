# Changelog

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1

- Added the candidate-driven search planner (`search::planner`, `candidates`, `dense`, `operator_first`, `scope`, `enrich`, `snippet`, `thread_sql`) and the `SearchStore` / `SearchRow` traits a host implements to run it against its own database. Additive; moved from MailRight (src-tauri db/search).

## 0.1.0 (unreleased)

- First RightKit release, moved from MailRight (src-tauri db/search query/terms/predicates/paging, db/semantic/fusion, trust, webimg dimensions/trackers).
