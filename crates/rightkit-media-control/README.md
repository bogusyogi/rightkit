# rightkit-media-control

Pauses other media players while a host records, and resumes only what it paused.

## Policy

- Recording start: for every player observed playing, send an explicit pause and remember (session id, player identity). Nothing playing means nothing is done.
- Recording stop, cancel or failure: for every player this session paused, send an explicit play only if that same player is still observed and is not playing again. Then forget it. A failed or empty observation never permits a command.
- Never toggle. Never mute or duck. No metadata beyond player identity.
- All commands run on one worker thread fed by a FIFO channel, so a stop that arrives during a slow start is processed after it.

## API

```rust
rightkit_media_control::set_event_sink(|event, detail| log(event, detail)); // optional, once
rightkit_media_control::install_backend(MyBackend);                         // optional, once
rightkit_media_control::begin_for_session(session_id);
rightkit_media_control::restore_for_session(session_id, "delivery_completed");
rightkit_media_control::restore_any("shutdown");
```

The `reason` argument is an outcome code written to the event sink.

## Backends

- Windows: built in, Global System Media Transport Controls through the `windows` crate.
- Other platforms: none built in. A host may install one by implementing `Backend` (`observe`, `pause`, `play`, each reporting `Confirmed`, `Ambiguous` or `Rejected`). With no backend every call does nothing.

## Host inputs

The event sink and, off Windows, a backend. The crate holds no product name, path or bundle id.

## Verification owed

Extracted from HeardRight `main` (2026-11-19) without a compile. Qualify the Windows backend before release.
