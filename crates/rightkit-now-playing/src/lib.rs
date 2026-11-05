//! Now-playing and media-keys owner (rightkit#959).
//!
//! One place to publish what is playing and to hear the media keys, over `souvlaki`: SMTC on Windows,
//! `MPNowPlayingInfoCenter` and `MPRemoteCommandCenter` on macOS, MPRIS on Linux. The Linux backend of
//! souvlaki is MPRIS itself (with volume), so `mpris-server` is not a second dependency.
//!
//! # The one-owner rule
//!
//! Exactly one thing owns the system media session of a process.
//!
//! - A system web view that plays media (a `<video>` or `<audio>` element in WebView2, WKWebView or
//!   WebKitGTK) owns the keys through the page's own Media Session API (`navigator.mediaSession`). The
//!   app must not register here for that playback: two registrations fight over the same keys and the
//!   Now Playing tile. [`Source::WebView`] is refused with [`Error::WebViewOwnsKeys`], so the rule fails
//!   loudly instead of silently double-registering.
//! - Playback the app does itself (a native player: PlayRight, a decoder feeding the audio device)
//!   registers once through [`NowPlaying::register`] with [`Source::Native`]. A second live
//!   registration in the same process is [`Error::AlreadyRegistered`].
//!
//! # Platform notes
//!
//! - macOS: the OS delivers remote commands on the main run loop, so the app needs a running
//!   `NSApplication` loop (a winit or Tauri app has one). No window is required.
//! - Windows: SMTC needs the app's top-level window: pass its `HWND` in [`Config::window_handle`].
//! - Linux: needs a session D-Bus. [`Config::dbus_name`] must follow the D-Bus bus-name rules
//!   (`[A-Za-z0-9_]` elements, for example `right_play`).
//! - `rightkit-automation::media_keys::send_media_key` is the opposite direction (it presses a key);
//!   it does not publish state.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// This process already holds a registration.
    #[error("a now-playing registration is already live in this process")]
    AlreadyRegistered,
    /// A system web view plays the media; its page's media session owns the keys.
    #[error("a web view that plays media owns the media keys through the page's media session; register only for native playback")]
    WebViewOwnsKeys,
    /// Windows needs the app window handle for SMTC.
    #[error("a window handle is required on Windows")]
    MissingWindowHandle,
    /// The OS backend refused (no session bus, no run loop, not allowed).
    #[error("media session backend: {0}")]
    Backend(String),
}

/// Who plays the media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The app decodes and plays it (a native player). The only source that may register.
    Native,
    /// A system web view plays it; its page owns the media session. Registering is refused.
    WebView,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Linux: the D-Bus name the player appears under (`right_play`).
    pub dbus_name: String,
    /// The name shown to the person in the OS media UI.
    pub display_name: String,
    /// Windows: the `HWND` of the app's top-level window, as an integer.
    pub window_handle: Option<usize>,
    pub source: Source,
}

impl Config {
    /// A native player with no window handle (macOS, Linux). Set [`Config::window_handle`] on Windows.
    pub fn native(dbus_name: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            dbus_name: dbus_name.into(),
            display_name: display_name.into(),
            window_handle: None,
            source: Source::Native,
        }
    }

    pub fn with_window_handle(mut self, hwnd: usize) -> Self {
        self.window_handle = Some(hwnd);
        self
    }
}

/// Cover art for the OS tile. The OS fetches it from the URL, so a local file becomes a `file://` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artwork {
    Url(String),
    File(PathBuf),
}

impl Artwork {
    fn url(&self) -> String {
        match self {
            Artwork::Url(url) => url.clone(),
            Artwork::File(path) => file_url(path),
        }
    }
}

/// What is playing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Track {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub artwork: Option<Artwork>,
    pub duration: Option<Duration>,
}

/// The playback state. `position` is the playhead when it is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    Paused { position: Option<Duration> },
    Playing { position: Option<Duration> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

/// A command from the OS: a media key, a lock-screen or tile button, or an MPRIS client.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    /// Seek by the platform's default step.
    Seek(Direction),
    SeekBy(Direction, Duration),
    SetPosition(Duration),
    /// 0.0..=1.0 (MPRIS only).
    SetVolume(f64),
    OpenUri(String),
    Raise,
    Quit,
}

fn direction(d: SeekDirection) -> Direction {
    match d {
        SeekDirection::Forward => Direction::Forward,
        SeekDirection::Backward => Direction::Backward,
    }
}

#[allow(unreachable_patterns)]
fn command(event: MediaControlEvent) -> Option<Command> {
    Some(match event {
        MediaControlEvent::Play => Command::Play,
        MediaControlEvent::Pause => Command::Pause,
        MediaControlEvent::Toggle => Command::Toggle,
        MediaControlEvent::Next => Command::Next,
        MediaControlEvent::Previous => Command::Previous,
        MediaControlEvent::Stop => Command::Stop,
        MediaControlEvent::Seek(d) => Command::Seek(direction(d)),
        MediaControlEvent::SeekBy(d, by) => Command::SeekBy(direction(d), by),
        MediaControlEvent::SetPosition(MediaPosition(at)) => Command::SetPosition(at),
        MediaControlEvent::SetVolume(v) => Command::SetVolume(v.clamp(0.0, 1.0)),
        MediaControlEvent::OpenUri(uri) => Command::OpenUri(uri),
        MediaControlEvent::Raise => Command::Raise,
        MediaControlEvent::Quit => Command::Quit,
        _ => return None,
    })
}

/// `file://` URL of a local path (percent-encoded; a Windows drive path becomes `file:///C:/...`).
pub fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let rooted = if text.starts_with('/') {
        text
    } else {
        format!("/{text}")
    };
    let mut out = String::from("file://");
    for byte in rooted.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

static LIVE: AtomicBool = AtomicBool::new(false);

struct Claim;

impl Claim {
    fn take() -> Result<Claim, Error> {
        LIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Claim)
            .map_err(|_| Error::AlreadyRegistered)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        LIVE.store(false, Ordering::Release);
    }
}

/// The process's registration with the OS media session. Dropping it detaches.
pub struct NowPlaying {
    controls: MediaControls,
    _claim: Claim,
}

fn backend(error: impl std::fmt::Debug) -> Error {
    Error::Backend(format!("{error:?}"))
}

impl NowPlaying {
    /// Registers for native playback and starts delivering commands to `handler`. The handler runs on a
    /// backend thread (or the main run loop on macOS): keep it short and send the command on to the
    /// player.
    pub fn register(
        config: Config,
        handler: impl Fn(Command) + Send + 'static,
    ) -> Result<Self, Error> {
        if config.source == Source::WebView {
            return Err(Error::WebViewOwnsKeys);
        }
        if cfg!(windows) && config.window_handle.is_none() {
            return Err(Error::MissingWindowHandle);
        }
        let claim = Claim::take()?;
        let platform = PlatformConfig {
            dbus_name: &config.dbus_name,
            display_name: &config.display_name,
            hwnd: config.window_handle.map(|h| h as *mut c_void),
        };
        let mut controls = MediaControls::new(platform).map_err(backend)?;
        controls
            .attach(move |event| {
                if let Some(command) = command(event) {
                    handler(command);
                }
            })
            .map_err(backend)?;
        Ok(Self {
            controls,
            _claim: claim,
        })
    }

    /// Publishes the track shown in the OS media UI.
    pub fn set_track(&mut self, track: &Track) -> Result<(), Error> {
        let cover = track.artwork.as_ref().map(Artwork::url);
        self.controls
            .set_metadata(MediaMetadata {
                title: track.title.as_deref(),
                album: track.album.as_deref(),
                artist: track.artist.as_deref(),
                cover_url: cover.as_deref(),
                duration: track.duration,
            })
            .map_err(backend)
    }

    /// Publishes play, pause or stop with the playhead.
    pub fn set_state(&mut self, state: State) -> Result<(), Error> {
        let playback = match state {
            State::Stopped => MediaPlayback::Stopped,
            State::Paused { position } => MediaPlayback::Paused {
                progress: position.map(MediaPosition),
            },
            State::Playing { position } => MediaPlayback::Playing {
                progress: position.map(MediaPosition),
            },
        };
        self.controls.set_playback(playback).map_err(backend)
    }

    /// Publishes the volume (0.0..=1.0). Only MPRIS has a volume; elsewhere this does nothing.
    pub fn set_volume(&mut self, volume: f64) -> Result<(), Error> {
        #[cfg(target_os = "linux")]
        {
            self.controls
                .set_volume(volume.clamp(0.0, 1.0))
                .map_err(backend)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = volume;
            Ok(())
        }
    }

    /// Clears the tile: playback `Stopped`, no track.
    pub fn clear(&mut self) -> Result<(), Error> {
        self.set_state(State::Stopped)?;
        self.set_track(&Track::default())
    }
}
