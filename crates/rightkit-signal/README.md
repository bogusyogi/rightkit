# rightkit-signal

Cross-process wake-up signals between a native companion and a host app, with one API on macOS and Windows (rightkit#867).

| Platform | Transport |
|---|---|
| macOS | Darwin notifications (`notify_post`, `notify_register_file_descriptor`) |
| Windows | Named auto-reset events, `Local\<name>` |
| other | every call answers `SignalError::Unsupported` |

```rust,ignore
use std::time::Duration;
use rightkit_signal::{post, Lease, LeaseMonitor, Watcher};

// Companion side: wake the host.
post("dev.orthic.pulse.hub.command")?;

// Host side: observe names, one thread, indexes into the list.
let watcher = Watcher::start(
    &["dev.orthic.pulse.hub.command", "dev.orthic.pulse.hub.show.overview"],
    |index, name| { /* drain the queue, show the section */ },
)?;

// Host: a visibility lease. Posts every 500 ms while the probe says the window is visible.
let lease = Lease::start("dev.orthic.pulse.hub.visible", Duration::from_millis(500), || window_is_visible())?;

// Companion: live while the last post is under 6 s old.
let monitor = LeaseMonitor::start("dev.orthic.pulse.hub.visible")?;
let hub_visible = monitor.is_live(Duration::from_secs(6));
```

- Signals carry no payload. Several posts before the handler runs are one call. A post while no watcher exists is lost: the reader reads the data it was told about when it starts. Put the data in a file or queue beside the signal, or use one name per thing.
- Names are 1 to 200 ASCII letters, digits, `.`, `_`, `-`. A watcher observes at most 63 names.
- A `Watcher` owns one thread blocked in `poll` (macOS) or `WaitForMultipleObjects` (Windows); a `Lease` owns one timer thread. Both are joined on drop.
- Not published (0.1.0, unreleased).
