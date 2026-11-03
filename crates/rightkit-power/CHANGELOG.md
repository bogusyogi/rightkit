# Changelog

## 0.1.0 (unpublished)

- New crate (#957): the one owner of the device's power and session state. `Power::spawn(wake)` starts the platform reader; `conditions()` (saver, on_battery, display_off, locked), `events()` (`Suspending(Ack)`, `Resumed`, `EndingSession(Ack)`, `Locked`, `Unlocked`), `on_suspending(hook)`, `take_suspended_at()`, `keep_awake(reason, kind)`. `Activity` keeps work out of App Nap on macOS. `PowerAssertion` and `PowerKind` are re-exported from `rightkit-child-process` (they stay there, not duplicated).
- No test double: there is no `Power::fixed`, `set_conditions` or `send`, and `keep_awake` returns `io::Result<PowerAssertion>` (never `None`). Adrian: "no mocks/fakes, no dummy data".
- macOS: sleep (`NSWorkspaceWillSleepNotification`), resume (`NSWorkspaceDidWakeNotification`), screen lock and unlock (the distributed notifications `com.apple.screenIsLocked` and `com.apple.screenIsUnlocked`), Low Power Mode read on demand in `conditions()`. The sleep `Ack` is not waited for (the notification is delivered on the main thread, where the app's own logic pass may be running).
- Windows: suspend and resume through `PowerRegisterSuspendResumeNotification` (the suspend and end-of-session callbacks wait up to 1.5 s for the app's `Ack`), AC or battery, console display state and battery saver through `PowerSettingRegisterNotification`, session lock and unlock through a message-only window and `WTSRegisterSessionNotification`.
- Linux: no reader yet; `Conditions::default()` and no events, and `Power::spawn` logs that state is unavailable. Not done: logind `PrepareForSleep`, `Lock` and `Unlock`.
- Not done: metered network, reachability and EcoQoS (not in this crate's first cut); macOS battery and display state.
- Design reference: `fastframe-power` (no code taken from it).
- Not compiled or run when written. Needs a device run on a Mac and a Windows machine: sleep and wake the machine, lock and unlock the session, and check `Power::events` and `conditions`.
