# rightkit-dictation-pill

Source intake for native dictation overlays & platform-neutral session logic. Build, runtime & installed qualification results are pending.

The controller behind a dictation pill: a recording capsule, a hint card and a clip card driven as three native layered, non-activating overlays on Windows (over `rightkit-overlay-win`), and the platform-neutral session logic around a pill of any kind: the event-driven full-screen and foreground guardian, the generation-stamped event queue, the capsule hitbox store and generic bounds helpers for a bridge to a native pill view. It carries no painters, layout sizes, colours, fonts, copy, easing curves, timings, tolerances, persisted keys, file names, tip logic or visibility policy: the host supplies all of those as required inputs.

`PillEvent` comes from `rightkit-dictation-core` 0.3.2. Checkout dependencies use version-compatible sibling paths; published packages resolve those versions through Cargo.

## Use (Windows controller)

```rust
use std::sync::Arc;
use rightkit_dictation_pill as pill;

// The look, the host's decisions and the callbacks are all required: none has a default.
let config = pill::Config::new(Arc::new(MyPainters::new()), my_host_inputs(), my_callbacks());
pill::install(config)?;             // once, before anything else; validates every input
let install = pill::ensure_installed(); // creates the three overlays and the owner thread (blocks, bounded)
// install.is_running() is true when a generation is up. Install::RetirementPending means an owner
// thread, window thread, event forwarder or worker of an earlier generation is still running:
// nothing was created, retry on the next start.
pill::apply_event(&pill::PillEvent::Recording, pill::Visibility::Show);
pill::voice_level(0.4);             // never wakes the driver
match pill::show_clipboard(items, true) {   // clip card
    Ok(()) => {}                    // the owner accepted the command (not: the card is on screen)
    Err(refusal) => {}              // a limit was broken, or NotRunning: nothing was shown
}
pill::shutdown();                   // destroys the overlays and stops the owner (bounded)
```

Until `install` has run every driver call does nothing. After `install`, only `ensure_installed` starts the driver and only `ensure_installed` and `shutdown` block. Every other call (events, getters, `show_*`) is **implicit**: it hands its command to a running owner or is dropped and reported (`call_dropped`); it never starts a generation and never waits for one, so it is safe on a UI thread. A dropped `show_clipboard` returns `Err(ClipRefusal::NotRunning)` and a dropped `show_hint` returns `false`, so the host never sets state for a card that is not showing. `is_running()` says whether a generation is up; a generation that failed or faulted stays dead until the host calls `ensure_installed` again, off its UI thread.

## Lifecycle

| Rule | Where |
| --- | --- |
| Each start is a generation with its own cancellation. The owner thread builds the shared state (so no host code runs on the caller's thread), checks the cancellation before each window, after each window and after each window's painter and forwarder thread, and must win a compare-and-swap before it publishes. A caller that times out (`Timings::startup_timeout_ms`), or a `shutdown` during startup, wins the opposite swap and the owner destroys what it made. A start that is cancelled while it waits for the previous owner spawns nothing. | `lifecycle.rs`, `driver.rs` `launch`, `build_surfaces`, `owner_main` |
| At most one owner thread is alive. Each owner has an exit signal; the latest is kept, and a new start and `shutdown` wait for it (bounded). `shutdown` returns `true` only when the latest owner finished AND every retained thread handle (the owner's and each window's, kept in `RETIRE`) finished AND every forwarder and worker retired: the exit signal alone proves neither the owner thread nor a window thread ended (`shutdown_incomplete` says which case). Windows are closed with `OverlayWindow::close_into_join_handle`, which posts the close and returns the thread's handle without waiting; handles are joined only once `is_finished()`, so nothing blocks on a wedged thread. `Timings::shutdown_timeout_ms` is at least 5 s, above the 4.5 s worst case of three windows. | `driver.rs` `LAST_EXIT`, `ExitSignal`, `RETIRE`, `shutdown` |
| A new generation is admitted only when the previous owner exited AND every retained thread handle (owner and windows) has finished AND no event forwarder or worker of any generation is alive. A finished window thread has destroyed its window (`hwnd` 0) and delivered `Closed`. Otherwise `ensure_installed` waits within one `Timings::shutdown_timeout_ms` bound and returns `Install::RetirementPending` having created nothing, so retries never accumulate threads or windows (at most one generation's 1 owner + 3 window handles are held). There is no permanent refusal. A host callback that never returns keeps `RetirementPending` for the process. Requires `rightkit-overlay-win` 0.1.6, which supplies `close_into_join_handle`. | `driver.rs` `launch`, `build_surfaces`, `forward_events`, `ForwarderLive`, `Made::drop`; `retire.rs` |
| Deadlines hold under load: the owner services its timers after at most 16 commands or 10 ms while a queue that never empties is drained, and a host result is resolved against its deadline at the moment the owner handles it, so one that arrives overdue is a failure (never "copied", a check mark or a close) whether or not the timer already ran. A token resolves once. | `driver.rs` `event_loop`, `on_timers`, `on_copy_done`, `on_action_done`, `Cmd::FlashDone`; `pending.rs` `Tracker::resolve` |
| A panic in `launch` is caught, the owner (ready or not) is told to shut down through its queue and the attempt counts as a failed start. A panic on the owner thread, a painter panic and an overlay window that ends by itself destroy the generation; an owner that dies between publishing its windows and being installed is marked dead right after. Only an explicit `ensure_installed` restarts. | `driver.rs` `start`, `run`, `Control::Fault`, `on_overlay` |
| The owner reads one bounded command queue; host commands beyond its capacity are dropped and reported once per burst; shutdown, a painter fault and a frame pump going idle are never dropped. | `cmd_queue.rs`, `driver.rs` `send`, `event_loop` |
| `shutdown` (returns `false`) and `ensure_installed` (returns `Install::Failed`) refuse when called from a host callback on the owner thread. | `driver.rs` `ON_OWNER` |

Guardian event drains process at most `EVENT_QUEUE_CAPACITY` queued events before servicing deadlines & rechecking stop. Host `ready` & `apply` panics are classified by callback & payload type; unexpected worker panics clear its generation slot & signal exit before reporting failure.

## What the crate owns

| Module | Owns |
| --- | --- |
| `pill`, `hint`, `clipboard` | Surface state: mode, hover, visibility, size spring, cross-fade, copy feedback, launch bloom and reveal, card entrance and exit, auto-hide and dismiss timers; `clipboard::validate_items`. |
| `driver` (Windows), `cmd_queue` | The owner thread, the three overlays, frame pumps, placement, topmost re-assertion, hover reconciliation, click dispatch, the pointer watches, the lifecycle; the bounded command queue. |
| `lifecycle`, `pending`, `retire`, `clock` | The generation and cancellation rules; the token + deadline tracker that keeps host work from being shown as done before the host reports it (and from being accepted after its deadline); the process-wide record of retained owner and window thread handles, live forwarders and running workers that gates a new generation; checked time arithmetic for host numbers. |
| `regions` | The interaction region contract and `cursor_at`. |
| `look` | `Painters`, `Metrics`, `Motion` and the frame and scene types. |
| `draw`, `text`, `anim`, `gfx` | Display-list types and stroke helpers, font slots, text measurement and wrapping, closed-form springs and fades (the easing curve is the host's), the overlay seam. |
| `visibility` | `Visibility` (show, hide, keep). Nothing else: the policy is the host's. |
| `guardian`, `guardian_worker`, `guardian_win`, `guardian_mac`, `fullscreen_mac` | The event-driven guardian: coalescing (host periods, `GuardianTimings`), the worker and its `GuardianHandle`, the OS event sources scoped to their generation, the host's `EdgeTolerance` for the macOS geometry fallback. (Windows full screen is `rightkit_overlay::windows::foreground_fullscreen_on`.) |
| `dispatch_queue`, `frame` | Generation-stamped, bounded event queue with panic-safe drain; capsule hitbox / frame store (cleared by `shutdown`) and aux-window geometry. |
| `bridge` | Generic helpers for a native view's C ABI: text cut at a character boundary, a log-safe filter, a name and an id alphabet, integer-code tables. No wire encoding, no splash logic. |
| `config` | `Config`, `HostInputs`, `Callbacks`, validation, the diagnostic sink. |

## The look: `Painters` (required)

`Config::new(painters, host, callbacks)` takes an `Arc<dyn Painters>`. The trait has no default methods.

| Method | Decides |
| --- | --- |
| `fonts()` | The five font slots (bytes, GDI and typographic names, hhea metrics). Read once at install; the crate registers the bytes privately per process and measures text against them. |
| `metrics()` | Pill window envelope, bottom margin, gap between capsule and cards, extent slack. Read once at install. |
| `motion()` | Every spring, fade length, easing curve (`fade_curve`, `crossfade_curve`) and duration the controller applies. Read once at install; the curves are probed once at install (0 -> 0, 1 -> 1). |
| `capsule_size(mode, hovered, clipboard_visible, label)` | The capsule size per mode. Called under the shared-state lock: it must be pure and fast. |
| `pill(scene, win_w, win_h, bottom_pad, pass)` | The capsule display list, its rectangle and its regions. `Pass::Layout` is the settled scene used for hit regions; it must not advance painter-side animation state. |
| `hint(view)` / `clip(view)` | A card's display list, window size, card rectangle and regions. |

The surface methods run on the overlay threads (paint) and the driver thread (layout), never while the shared-state lock is held. A panic in any look method destroys the generation. What a look returns is sanitised: sizes clamped to `1..=4096` points, non-finite numbers count as zero, at most 64 regions and 4096 display-list operations per frame, polygons of at most 256 points, text runs of at most 4096 bytes.

## Interaction regions

```rust
Region::new(RegionId(7), rect, HoverRole::Control)   // RegionId is the painter's own number
    .cursor(Cursor::Pointer)                          // see "Cursor": inert today
    .action(Action::Host(ActionTag(4)))               // what a click does
    .closing_clip_on_success()                        // optional: close the open clip card once the host reports Done
    .closing_card_after()                             // optional: close the owning card after the action
    .secondary(ActionTag(200))                        // optional: right-button press action
```

- `HoverRole::Control`: highlightable; pointer over it counts as over the surface. `HoverRole::Body`: counts as over the surface, no highlight, swallows clicks. A card lists its `Body` region over the card first, then its controls (later regions win overlaps).
- `Shape::Rect` only. Ids at or above `RegionId::RESERVED_FROM` (0xFFFF_FF00) are the crate's.
- `Action`: `None`, `Dismiss`, `CopyCurrent` / `CopyItem(i)` / `ToggleAlternate` (optional clip-card behaviour over opaque `primary` / `alternate` payloads), `Host(tag)`, `HostFlash(tag)`.
- A click on the pill is logged as the region's number (`region=<id>`), never as a name.
- Copy & host-action success states wait for host completion callbacks. A late answer is ignored.

## Cursor

`Region::cursor` is carried and compared but **not applied**: `rightkit-overlay-win` 0.1.6 registers its window class with the arrow cursor and has no `WM_SETCURSOR` hook. The driver reports `cursor_inert` once per generation when a frame asks for a non-default cursor. Region cursors require an overlay API that applies each requested cursor.

## Host inputs and bounds

`HostInputs`, `Callbacks`, `look::Motion` and `GuardianTimings` have no `Default`. `config::install` validates every number, string, list, duration and curve and refuses with `InstallError::Invalid(<field>)` before anything is stored; Input ranges are documented beside their fields & enforced by `config::install`. What the host sends at run time is bounded too: labels and hint text are cut to `Limits`, a clip card that breaks a limit (one payload, or all payloads together) is refused (not cut, so a copy is never truncated), worker threads are capped process-wide, the command queue and the guardian's event channel are bounded, text measured or wrapped is bounded, and every host duration goes through checked arithmetic.

## Diagnostics

Event names are `<HostInputs::event_prefix>_<code>`; the detail is `key=value` text made only of fixed codes, booleans, counts, the closed surface names (`Pill`, `Hint`, `Clip`) and numbers (generations, deadlines, region ids). It never carries a host string (a window name, a hint kind, a region name), a transcript, a label, a path or an error text. A panic is reported by the callback's name and the payload's type (`str`, `string`, `other`), never its text; a host `Err(String)` does not exist (callbacks answer `bool` / `ActionOutcome`).

## Platforms

`driver`, `gfx` and `guardian_win` are Windows; `guardian_mac` and `fullscreen_mac` are macOS; everything else is platform-free. A macOS pill is a native view the host drives through its own C ABI; the crate has no macOS overlay backend and no wire protocol for it.

## Verification owed

Run native Windows & macOS qualification against installed consumers: startup cancellation, repeated restart, shutdown with retained threads, clipboard completion deadlines, guardian event floods & callback panics, foreground/fullscreen transitions, & host-supplied painters, labels & themes.
