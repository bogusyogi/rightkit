# rightkit-overlay

Portable overlay state, geometry, work-area clamping, hover & hit testing, plus Windows native style/window primitives. Callers supply their overlay kind & native presentation policy. Crate owns no event loop, timer or rendering content.

`OverlayModel<Kind>` retains caller-owned kind without interpreting it. Use any serializable/deserializable identity, including app enum with existing serde wire names. Native hosts explicitly select `OverlayStyle`, fullscreen policy & platform window level. Windows PASSIVE remains topmost/no-activate/toolwindow; click-through creation remains refused because unlayered native creator cannot provide that behavior.

```rust
use rightkit_overlay::{OverlayEvent, OverlayModel, Rect, WorkArea};
let mut model = OverlayModel::new("compact-status",
    Rect { x: 0.0, y: 0.0, width: 320.0, height: 48.0 });
model.apply(OverlayEvent::Idle);
model.place(WorkArea { left: 0.0, top: 0.0, right: 1440.0, bottom: 900.0 });
assert!(model.hit_test(10.0, 10.0));
```

## Migration to 0.2.0

Removed `OverlayKind` product variants. Define equivalent kinds in app configuration; retain `#[serde(rename_all = "snake_case")]` on caller enum to preserve old wire strings. Type annotations become `OverlayModel<AppOverlayKind>`. `OverlayModel::new(app_kind, frame)` still initializes hidden state, with unchanged events, clamping & hit testing.

Removed `HR_STATUS_WINDOW_LEVEL`. Native macOS host now supplies its own declared level; configure `25` to preserve previous status-level behavior. Crate never called AppKit or applied this constant. Windows callers continue passing explicit `OverlayStyle`; no native creation/style default changes.

## Platforms & tests

No Cargo features or new dependencies. Windows native operations use caller-owned HWNDs. `windows::create_overlay_window` creates hidden unlayered windows for caller-registered class; caller owns destruction. `make_noactivate` installs mouse guard on owner thread. `show_noactivate`, `hide`, topmost repair, monitor/DPI & fullscreen helpers retain existing contracts. Non-Windows native module returns existing false/None/zero stubs; portable policy works on every target.

Unit tests cover caller kinds, state, geometry, hover, style, fullscreen & stacking. `tests/windows_native.rs` requires managed interactive Windows desktop/monitor, creates private class/window, checks hidden creation, actual native styles/geometry, show/hide with unchanged foreground, & destruction. RAII destroys/unregisters on assertion unwind. Integration owner records native smoke output; source presence is not native qualification.
