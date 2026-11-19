# rightkit-hotkeys

Native global shortcuts, configurable push-to-talk & middle-click handling for desktop hosts.

## APIs

- `chord`, `conflict`, `hold`, `registration`, `register_status`, `hook_chord`: portable chord parsing, conflicts, registration & press/release policy.
- `tap_hold`: host-configured tap/hold, debounced & immediate chords through native `GlobalHotkeys`. At most 64 bindings, 8 start jobs & 32 end jobs; each registration owns one bounded report lane.
- `macos`: Carbon hotkeys, CoreGraphics fn/PTT & middle-button taps. `holders::{FnHotkeyHolder, MiddleMouseHolder}` retain host callbacks/settings & expose `stop()`.
- `windows`: `RegisterHotKey`, low-level keyboard/mouse primitives & optional `hub`; `input_hooks::InputHooks` composes `ptt_hook::KeyboardState` & `mouse_hook::MiddleClickState` on one hook thread with separate dispatch/cancel/report lanes.

Windows `InputHooks` & `hub` claim identical process slots; use one. Platform modules are cfg-gated; portable policy modules remain available elsewhere. Windows requires published `rightkit-automation >= 0.7.1` for tri-state UIA link lookup & tagged input batches, plus `windows-sys 0.61.2`.

## Host policy & injected input

Hosts supply chord/key choices, timing, enabled state, callbacks, sink, main-thread runner & middle-click replay tag. Input handling serves user-configured local shortcuts, PTT & middle-click actions; it stores no text or input history. Diagnostic text reports key classes, lifecycle outcomes & counts, without key identities.

`KeyboardState` passes every injected keyboard event unchanged before gesture tracking; synthetic command-delivery events cannot arm, cancel or modify PTT state. `MiddleClickState` passes events marked injected **&** carrying its host-supplied replay tag unchanged. Other injected mouse events follow normal enabled middle-button policy, preserving native remote-input behavior. Hosts choose replay tags through `MiddleClickState::new(tag)` or `with_timing(tag, timing)`; tags are loop prevention, not authentication. Low-level primitives expose injected flags to callers. Existing macOS tap event rules remain unchanged.

## Lifecycle

Windows install generations fence timer posts, queued starts & worker results. Swallowed presses retain release debt until their matching release or hook removal, including teardown. Keyboard callbacks run off hook thread; cancel has its own lane. Middle-click work has one outstanding press, bounded queues & capped detached lookups. Only confirmed non-links toggle; unknown lookup results replay after target validation. Replay batches preserve original press point & carry matching down/up events.

`InputHooks::drop` quiesces timer posting, then waits at most 1.5 s for helper-thread hook removal; quiesce itself waits for an in-flight post. If helper creation fails, removal runs inline & may wait without bound. `InputHooks::end` transfers teardown to a helper; its fallback also runs inline if helper creation fails. Stalled host callbacks/provider calls cannot be forcibly interrupted: capped workers may remain until they return; a new install is refused while old hook removal remains pending. Hook removal while a swallowed key/button remains held can expose its eventual release without its press, reported through owed-release diagnostics.

macOS holders disable starts before helper-thread tap disposal, wait at most 1.5 s & retain end callbacks. Helper creation failure may dispose inline without bound. AX lookups are capped at two in flight. Existing macOS middle-click primitives retain coordinate-only replay, lookup-error behavior & their original native event rules. Holder fencing prevents starts after stop; underlying run-loop disposal can outlive its wait.

Carbon creation/release stays on main thread. Main-thread registration executes inline; queued timeout cancels pending work, while an already-running registration returns its actual ownership result. Failed unregister scheduling retains owned registrations for retry. Host callbacks already executing finish under their existing ownership.

## Qualification

This intake carries donor revision 5.3.2, including rev57's source-reviewed rebound-key release correction. Build, installed Windows/macOS shortcut journeys, blocked-lookup teardown, key-release pairing, replay targeting & repeated stop/restart remain integration-owner qualification work. Package resource metadata records thread/lane bounds & release paths. Changes: [CHANGELOG.md](CHANGELOG.md).
