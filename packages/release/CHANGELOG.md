# Changelog

## 0.2.137

- `right-release ownership --advisory-when-pending` exits 0 only while approval-key enrollment is pending (findings still print as diagnostics; JSON keeps `ok: false`, `enrollmentPending: true`). Used by right-git's `ci` lane; release lanes never pass it.

## 0.2.136 — 2026-10-09

- Add `right-release install-app --dmg <path.dmg>`: sanctioned, non-intrusive macOS install of a signed DMG. Verifies signature, Gatekeeper, notarization staple, bundle id and Team ID before touching the installed app; quits via signals (no AppleEvents); replaces via Trash with rollback; relaunches hidden and in the background only. Windows exits "not implemented on win32".
