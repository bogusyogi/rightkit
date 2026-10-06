# rightkit-computer — DEPRECATED

Folded into [`rightkit-automation`](../rightkit-automation) (2026-10-06). Do not start new work here and do not add a dependency on this crate.

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

The crate stays in the workspace only so existing path references keep building; it will be removed once nothing references it. It is `publish = false` and was never published.
