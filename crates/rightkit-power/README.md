# rightkit-power

One owner of the device's power and session state for Right Suite apps: sleep and resume
(with a bounded acknowledgement before the system sleeps), session lock, battery or AC,
energy saver, display off, and keeping the computer awake.

**Use it** for RightPass lock on sleep or session lock, HeardRight reconnect after wake,
PlayRight pause on sleep.
**Do not use it** to decide app policy: it reports state and events, the app decides.

```rust
use rightkit_power::{Event, Power};

let power = Power::spawn(|| { /* ask for the next logic pass */ });
for event in power.events() {
    match event {
        Event::Suspending(ack) | Event::EndingSession(ack) => { /* save */ ack.done(); }
        Event::Resumed => { /* reconnect what the sleep broke */ }
        Event::Locked => { /* lock the vault */ }
        Event::Unlocked => {}
    }
}
```

`PowerAssertion` and `PowerKind` (keep awake) are
re-exported from `rightkit-child-process`.

## Platform support

| Capability | macOS | Windows | Linux |
|---|---|---|---|
| Suspending, Resumed | yes (the `Ack` is not waited for) | yes (waits up to 1.5 s) | not yet |
| Locked, Unlocked | screen lock notifications | session notification window | not yet |
| saver | Low Power Mode, read in `conditions()` | battery saver | not yet |
| on_battery, display_off | no | yes | not yet |
| `Activity` (App Nap) | yes | no-op | no-op |

A platform that cannot report a field leaves its default, the unconstrained value.
