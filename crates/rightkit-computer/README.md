# rightkit-computer (DEPRECATED)

Shared computer-control layer for observing and acting on a desktop app window in the background (macOS prototype). Catalog capability: `computer-control`.

**Deprecated.** The crate was folded into [`rightkit-automation`](../rightkit-automation) (2026-10-06). Do not start new work here and do not add a dependency on this crate. It stays in the workspace only so existing path references keep building.

## What replaced it

Everything it contained exists in `rightkit-automation` with the same behaviour, plus a Windows backend, stable element ids, the freshness guard, the four-tier input ladder, per-tier evidence, lease fencing, an admission hook and screenshot redaction:

| `rightkit-computer` | `rightkit-automation` |
|---|---|
| `safety::Policy` (allowlist), `kill()` | `Policy`, `kill()` / `reset_kill()` / `killed()` (adds deny list, self-protection, risk classes) |
| `model::{WindowInfo, Node, Status, Error}` | `model::*` (stable ids, `Snapshot`, typed `Error`) |
| `macos::list_windows`, `frontmost_pid` | `Automation::windows`, `Automation::frontmost` |
| `macos::observe_tree` | `Automation::snapshot` |
| `macos::El::{press, set_value, press_menu}` | `Action::{Press, SetValue, MenuPress}` through `Automation::act` (tier 1) |
| `macos::type_text_to_pid` | `Action::TypeText` / `Action::Keys` (tier 3, per-pid events) |
| `macos::capture_window` | `Automation::screenshot` (now redacted) |
| `examples/textedit_proof.rs` | `rightkit-automation/tests/e2e_mac.rs` |

## Cargo features

None. The crate has no `[features]` table and no dependencies.

## Usage

Not for new code. The workflow is in `rightkit-automation`. The one example left here is `examples/textedit_proof.rs`.

## Platform support

macOS only, as a prototype. The module `macos.rs` binds the system frameworks directly. `Cargo.toml` says a Windows backend would land later behind `cfg(windows)`; this crate has none.

## Version and changes

Not published (`publish = false`). See [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.
