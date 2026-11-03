// rightkit-now-playing 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Registering a native session would attach to the real OS media controls (SMTC, MPNowPlayingInfoCenter,
// MPRIS), so this consumer never does. It exercises the pure helpers, the data types and the two
// registration refusals that happen before any OS call.
use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_now_playing::{
    file_url, Artwork, Command, Config, Direction, Error, NowPlaying, Source, State, Track,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    // file:// URLs: percent-encoding of spaces and non-ASCII bytes, Windows drive paths, rooted paths.
    check(
        "unix path with space and non-ASCII",
        file_url(Path::new("/tmp/a b/\u{e9}.png")) == "file:///tmp/a%20b/%C3%A9.png",
    );
    check(
        "windows drive path",
        file_url(Path::new("C:\\Music\\x.mp3")) == "file:///C:/Music/x.mp3",
    );
    check(
        "unreserved characters stay",
        file_url(Path::new("/a-b_c.d~e")) == "file:///a-b_c.d~e",
    );

    // Configuration: a native player has no window handle until one is given.
    let config = Config::native("right_play", "Right Play");
    check(
        "native config",
        config.dbus_name == "right_play"
            && config.display_name == "Right Play"
            && config.window_handle.is_none()
            && config.source == Source::Native,
    );
    check(
        "window handle is carried",
        config.clone().with_window_handle(0x1234).window_handle == Some(0x1234),
    );

    // Data types.
    let track = Track::default();
    check(
        "default track is empty",
        track.title.is_none()
            && track.artist.is_none()
            && track.album.is_none()
            && track.artwork.is_none()
            && track.duration.is_none(),
    );
    let titled = Track {
        title: Some("Song".into()),
        artwork: Some(Artwork::File(PathBuf::from("/tmp/cover.png"))),
        duration: Some(Duration::from_secs(180)),
        ..Track::default()
    };
    check(
        "track fields",
        titled != track && titled.title.as_deref() == Some("Song"),
    );
    check(
        "artwork variants differ",
        Artwork::Url("https://example.com/c.png".into()) != Artwork::File(PathBuf::from("/c.png")),
    );
    check(
        "state carries the playhead",
        State::Playing {
            position: Some(Duration::from_secs(5)),
        } != State::Paused {
            position: Some(Duration::from_secs(5)),
        } && State::Stopped == State::Stopped,
    );
    check(
        "command values compare",
        Command::SeekBy(Direction::Forward, Duration::from_secs(10))
            == Command::SeekBy(Direction::Forward, Duration::from_secs(10))
            && Command::Seek(Direction::Forward) != Command::Seek(Direction::Backward)
            && Command::SetVolume(0.5) == Command::SetVolume(0.5),
    );

    // The one-owner rule: a web view source is refused before the backend is touched.
    let web = Config {
        source: Source::WebView,
        ..Config::native("right_play", "Right Play")
    };
    let refused = NowPlaying::register(web, |_| {});
    check(
        "web view registration refused",
        matches!(refused, Err(Error::WebViewOwnsKeys)),
    );
    check(
        "web view error explains the owner",
        Error::WebViewOwnsKeys.to_string().contains("media session"),
    );
    check(
        "backend error text",
        Error::Backend("x".into()).to_string() == "media session backend: x",
    );
    check(
        "already registered text",
        Error::AlreadyRegistered
            .to_string()
            .contains("already live"),
    );

    // Windows needs the top-level window for SMTC and refuses before the backend is touched.
    #[cfg(windows)]
    check(
        "windows requires a window handle",
        matches!(
            NowPlaying::register(Config::native("right_play", "Right Play"), |_| {}),
            Err(Error::MissingWindowHandle)
        ),
    );

    println!("OK rightkit-now-playing 0.1.0 (registry consumer, public API)");
}
