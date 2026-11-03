# Changelog

## 0.1.2 (unpublished)

- rightkit#1499, Adrian's rule "unknown liveness is never eligible for cleanup":
  - S3: the Windows held-open probe walks every file under a folder candidate (it already did; now also bounded by time, 30 s). A hit bound, an unreadable entry or a file that cannot be proven closed makes the folder ineligible.
  - S4: a Windows age rule with no owner (temp, downloads, crash dumps, error reports) is never eligible: it is reported with "Can't tell which app owns this", never preselected, and refused at apply.
  - S9 (breaking): `apply`, `apply_with`, `apply_run` and `apply_run_with_rules` take `running: &mut RunningProvider<'_>` (`FnMut() -> Result<Vec<String>, String>`) instead of `&[String]`. It is called again for every item, right before that item is re-checked; an `Err` skips the item as unknown liveness. `running_process_list()` is the ready provider answer (`Err` on an empty list). `scan` still takes `&[String]`.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (`pulse-core`: scan, rules, cleanup, duplicates, folder growth, history, store, snapshot, activity, storage browser, platform identity).
- The cleanup scan's own state folder is a parameter (`cleanup_scan::set_state_app_name`) instead of a fixed product name.
