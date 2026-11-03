# rightkit-now-playing

The now-playing and media-keys owner of the suite: publish what is playing and hear the media keys, over `souvlaki` (SMTC on Windows, Now Playing on macOS, MPRIS on Linux). Needed by `@rightkit/media-ui`, CutRight, ViewRight and PlayRight's native player. `rightkit-automation::media_keys::send_media_key` only presses a key; nothing before this published state.

Catalog capability: `now-playing`. Not yet published (0.1.0, unreleased). Rightkit#959.

```rust
use rightkit_now_playing::{Artwork, Command, Config, NowPlaying, State, Track};
use std::time::Duration;

let config = Config::native("right_play", "PlayRight"); // Windows: .with_window_handle(hwnd)
let mut now = NowPlaying::register(config, |command| match command {
    Command::Toggle => { /* player.toggle() */ }
    Command::Next => { /* player.next() */ }
    _ => {}
})?;
now.set_track(&Track {
    title: Some("Song".into()),
    artist: Some("Artist".into()),
    artwork: Some(Artwork::File("/path/cover.jpg".into())),
    duration: Some(Duration::from_secs(215)),
    ..Track::default()
})?;
now.set_state(State::Playing { position: Some(Duration::ZERO) })?;
```

## The one-owner rule

Exactly one thing owns the system media session of a process.

- **A system web view that plays media owns the keys** through the page's own Media Session API (`navigator.mediaSession`). The app registers nothing here for that playback. `Source::WebView` is refused with `Error::WebViewOwnsKeys`, so the rule fails loudly.
- **Native playback registers once** with `Source::Native` (the default of `Config::native`). A second live registration in the process is `Error::AlreadyRegistered`.

## Entry points

`NowPlaying::{register, set_track, set_state, set_volume, clear}`, `Config`, `Source`, `Track`, `Artwork` (`Url` or `File`, a file becomes a percent-encoded `file://` URL via `file_url`), `State`, `Command` (`Play`, `Pause`, `Toggle`, `Next`, `Previous`, `Stop`, `Seek`, `SeekBy`, `SetPosition`, `SetVolume`, `OpenUri`, `Raise`, `Quit`), `Direction`, `Error`.

## Platform notes

- macOS: needs a running `NSApplication` main run loop (winit and Tauri have one); no window is needed.
- Windows: pass the top-level window `HWND` (SMTC); without it `register` returns `Error::MissingWindowHandle`.
- Linux: needs a session D-Bus; `dbus_name` follows the D-Bus bus-name rules. souvlaki's Linux backend is MPRIS (with volume), so `mpris-server` is not a second dependency; built with souvlaki's `use_zbus` backend, so no system libdbus is linked.
- Volume is published on Linux only; `set_volume` does nothing elsewhere.

## Not verified here

This crate was written against the souvlaki 0.8.3 docs and has not been compiled or run on a device. Acceptance needs one real run per OS: the OS media UI shows the track and the media keys reach the handler (macOS, Windows, Linux).
