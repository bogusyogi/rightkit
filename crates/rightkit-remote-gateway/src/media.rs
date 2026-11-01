//! `media.control`: a remote caller plays, pauses, skips, starts music and reads what is playing
//! in Spotify or Music. `now_playing` returns the state and the title, artist and album only.
//! Tier 2, its own switch (`remoteMedia`, off), needed in addition to `remoteTools`. The verbs are
//! low risk (they change what plays, nothing on disk), so they need no per-call approval.
//!
//! * macOS: Apple Events through `/usr/bin/osascript`. Every verb is a fixed script written in
//!   this file; the only text from the caller is the search query, passed as an argument and
//!   never part of script text. The script never starts the app (`is running` is asked first,
//!   `not_running` otherwise: open it with `apps.launch`). The Automation prompt belongs to
//!   The host app (`NSAppleEventsUsageDescription`); a refusal of it is `permission_missing`.
//! * Windows: the system media session (`GlobalSystemMediaTransportControlsSessionManager`,
//!   SMTC) through the `windows` crate: play, pause, next, previous on the session whose source
//!   app is Spotify or Apple Music.
//! * `search_play`: Music on macOS plays the first library track whose name, artist or album
//!   contains the query. Spotify has no scripting verb that searches, so on both systems the
//!   query opens Spotify's own search page (`spotify:search:<query>`); the answer says
//!   `played: false, opened_search: true`. Apple Music on Windows does not support it.

use super::settings::{Settings, hard_floor};
use super::{Origin, Refusal, Result};
use serde_json::{Value, json};

const QUERY_MAX: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum App {
    Spotify,
    Music,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Spotify => "spotify",
            App::Music => "music",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verb {
    Play,
    Pause,
    Next,
    Previous,
    SearchPlay,
    NowPlaying,
}

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

/// The `now_playing` answer: `state` (`playing`, `paused`, `stopped` or `unknown`) and, when there
/// is a track, its title, artist and album. `rest` is those three lines in that order.
#[cfg(any(target_os = "macos", windows))]
fn track_answer(state: &str, rest: &[&str]) -> Value {
    if rest.len() < 3 {
        return json!({"state": state});
    }
    json!({"state": state, "title": rest[0], "artist": rest[1], "album": rest[2]})
}

/// `media.control`. `args`: `verb` (`play`, `pause`, `next`, `previous`, `search_play`,
/// `now_playing`), `app` (`spotify` or `music`, default `spotify`), `query` (`search_play` only,
/// at most 200 characters).
pub fn control(settings: &Settings, _origin: Origin, args: &Value) -> Result<Value> {
    // Nothing types, launches or takes focus while the user holds the screen (spot takeover, #355).
    super::screen::user_in_control()?;
    settings.require_media()?;
    let verb = match args["verb"].as_str() {
        Some("play") => Verb::Play,
        Some("pause") => Verb::Pause,
        Some("next") => Verb::Next,
        Some("previous") => Verb::Previous,
        Some("search_play") => Verb::SearchPlay,
        Some("now_playing") => Verb::NowPlaying,
        _ => {
            return Err(invalid(
                "verb is play, pause, next, previous, search_play or now_playing",
            ));
        }
    };
    let app = match args["app"].as_str() {
        None | Some("spotify") => App::Spotify,
        Some("music") => App::Music,
        Some(_) => return Err(invalid("app is spotify or music")),
    };
    // The owner's blocklist (`remoteBlockedApps`) and the hard floor apply to media apps too.
    let ids: &[&str] = match app {
        App::Spotify => &["com.spotify.client", "spotify.exe"],
        App::Music => &["com.apple.Music", "AppleInc.AppleMusic", "iTunes.exe"],
    };
    if ids
        .iter()
        .any(|id| hard_floor(id) || settings.app_blocked(id))
    {
        return Err(Refusal::new(
            "app_blocked",
            "the owner blocked that app (remoteBlockedApps)",
        ));
    }
    let query = match (&args["query"], verb) {
        (Value::Null, Verb::SearchPlay) => {
            return Err(invalid("search_play needs a query"));
        }
        (Value::Null, _) => None,
        (_, v) if v != Verb::SearchPlay => {
            return Err(invalid("query is for search_play only"));
        }
        (Value::String(q), _) => {
            let q = q.trim();
            if q.is_empty() || q.chars().count() > QUERY_MAX || q.chars().any(char::is_control) {
                return Err(invalid(
                    "query is 1 to 200 characters with no control characters",
                ));
            }
            Some(q.to_string())
        }
        _ => return Err(invalid("query must be a string")),
    };
    let outcome = imp::run(app, verb, query.as_deref())?;
    let mut answer = json!({"done": true, "app": app.name()});
    answer["verb"] = json!(match verb {
        Verb::Play => "play",
        Verb::Pause => "pause",
        Verb::Next => "next",
        Verb::Previous => "previous",
        Verb::SearchPlay => "search_play",
        Verb::NowPlaying => "now_playing",
    });
    if verb == Verb::SearchPlay {
        answer["played"] = json!(outcome.played);
        answer["opened_search"] = json!(outcome.opened_search);
    }
    if verb == Verb::NowPlaying {
        answer["track"] = outcome.track.unwrap_or_else(|| json!({"state": "unknown"}));
    }
    Ok(answer)
}

/// What a verb did: `search_play` sets `played` and `opened_search`; `now_playing` sets `track`.
struct Outcome {
    played: bool,
    opened_search: bool,
    track: Option<Value>,
}

impl Outcome {
    #[allow(dead_code)]
    const DONE: Outcome = Outcome {
        played: false,
        opened_search: false,
        track: None,
    };
}

/// Percent-encodes a query for a `spotify:search:` URI: every byte but an unreserved one.
fn encode_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len() * 3);
    for byte in query.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::*;

    pub fn run(_: App, _: Verb, _: Option<&str>) -> Result<Outcome> {
        Err(Refusal::new(
            "unsupported_platform",
            "media.control runs on macOS and Windows only",
        ))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use rightkit_child_process::RunOptions;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(10);

    /// One fixed script per app and verb: the bundle id is a literal in this file.
    macro_rules! tell {
        ($bundle:literal, $body:literal) => {
            concat!("tell application id \"", $bundle, "\" to ", $body)
        };
    }
    macro_rules! running {
        ($bundle:literal) => {
            concat!("return application id \"", $bundle, "\" is running")
        };
    }

    const SPOTIFY_RUNNING: &str = running!("com.spotify.client");
    const MUSIC_RUNNING: &str = running!("com.apple.Music");

    /// The state, then title, artist and album on their own lines (`track_answer` reads them).
    const SPOTIFY_NOW: &str = "tell application id \"com.spotify.client\"\n\
        if player state is stopped then return \"stopped\"\n\
        set t to current track\n\
        return (player state as string) & linefeed & (name of t) & linefeed & (artist of t) & linefeed & (album of t)\n\
        end tell";
    const MUSIC_NOW: &str = "tell application id \"com.apple.Music\"\n\
        if player state is stopped then return \"stopped\"\n\
        set t to current track\n\
        return (player state as string) & linefeed & (name of t) & linefeed & (artist of t) & linefeed & (album of t)\n\
        end tell";

    /// The Music search script. The query arrives as `item 1 of argv` with a fixed `q` in front of
    /// it (so an argument that starts with a dash is never read as an option); it is data, never
    /// script text.
    const MUSIC_SEARCH: &str = "on run argv\n\
        set q to text 2 thru -1 of (item 1 of argv)\n\
        tell application id \"com.apple.Music\"\n\
        set hits to (every track of library playlist 1 whose name contains q or artist contains q or album contains q)\n\
        if (count of hits) is 0 then return \"none\"\n\
        play (item 1 of hits)\n\
        return \"played\"\n\
        end tell\n\
        end run";

    fn script(app: App, verb: Verb) -> &'static str {
        match (app, verb) {
            (App::Spotify, Verb::Play) => tell!("com.spotify.client", "play"),
            (App::Spotify, Verb::Pause) => tell!("com.spotify.client", "pause"),
            (App::Spotify, Verb::Next) => tell!("com.spotify.client", "next track"),
            (App::Spotify, Verb::Previous) => tell!("com.spotify.client", "previous track"),
            (App::Music, Verb::Play) => tell!("com.apple.Music", "play"),
            (App::Music, Verb::Pause) => tell!("com.apple.Music", "pause"),
            (App::Music, Verb::Next) => tell!("com.apple.Music", "next track"),
            (App::Music, Verb::Previous) => tell!("com.apple.Music", "previous track"),
            (App::Music, Verb::SearchPlay) => MUSIC_SEARCH,
            // Spotify's search is not a script (see `run`); this is never run.
            (App::Spotify, Verb::SearchPlay) => SPOTIFY_RUNNING,
            (App::Spotify, Verb::NowPlaying) => SPOTIFY_NOW,
            (App::Music, Verb::NowPlaying) => MUSIC_NOW,
        }
    }

    /// Runs `osascript -e <script> [-- argument]` with a timeout; the script's output on success.
    fn osascript(script: &str, argument: Option<&str>) -> Result<String> {
        let mut args = vec!["-e", script];
        if let Some(argument) = argument {
            args.extend(["--", argument]);
        }
        let options = RunOptions::new(TIMEOUT).stdout_cap(4096).stderr_cap(4096);
        let done = rightkit_child_process::run("/usr/bin/osascript", args, &options)
            .map_err(|_| Refusal::new("spawn_failed", "osascript could not be started"))?;
        if done.timed_out {
            return Err(Refusal::new(
                "timed_out",
                "the app did not answer in 10 seconds",
            ));
        }
        let stdout = String::from_utf8_lossy(&done.stdout.bytes).into_owned();
        if done.status.success() {
            return Ok(stdout.trim().to_string());
        }
        let stderr = String::from_utf8_lossy(&done.stderr.bytes).into_owned();
        if stderr.contains("-1743") || stderr.contains("-1744") {
            return Err(Refusal::new(
                "permission_missing",
                "The app is not allowed to control that app; allow it in System Settings, Privacy & Security, Automation",
            ));
        }
        Err(Refusal::new(
            "act_failed",
            "the app did not accept the command",
        ))
    }

    fn require_running(app: App) -> Result<()> {
        let probe = match app {
            App::Spotify => SPOTIFY_RUNNING,
            App::Music => MUSIC_RUNNING,
        };
        if osascript(probe, None)? == "true" {
            Ok(())
        } else {
            Err(Refusal::new(
                "not_running",
                "that app is not running; open it first with apps.launch",
            ))
        }
    }

    pub fn run(app: App, verb: Verb, query: Option<&str>) -> Result<Outcome> {
        require_running(app)?;
        match (app, verb) {
            (App::Spotify, Verb::SearchPlay) => {
                let encoded = encode_query(query.unwrap_or_default());
                // `open_url` reports a launcher that exited with a failure as `ErrorKind::Other`
                // (CP/src/launch.rs `hand_off`); anything else is a launcher that did not start.
                rightkit_child_process::open_url(&format!("spotify:search:{encoded}")).map_err(
                    |e| {
                        if e.kind() == std::io::ErrorKind::Other {
                            Refusal::new("act_failed", "Spotify did not open the search")
                        } else {
                            Refusal::new("spawn_failed", "open could not be started")
                        }
                    },
                )?;
                Ok(Outcome {
                    played: false,
                    opened_search: true,
                    track: None,
                })
            }
            (App::Music, Verb::SearchPlay) => {
                let argument = format!("q{}", query.unwrap_or_default());
                match osascript(script(app, verb), Some(&argument))?.as_str() {
                    "played" => Ok(Outcome {
                        played: true,
                        opened_search: false,
                        track: None,
                    }),
                    _ => Err(Refusal::new(
                        "not_found",
                        "no track in the Music library matches that query",
                    )),
                }
            }
            (_, Verb::NowPlaying) => {
                let text = osascript(script(app, verb), None)?;
                let lines: Vec<&str> = text.lines().collect();
                let state = lines.first().copied().unwrap_or("unknown");
                Ok(Outcome {
                    played: false,
                    opened_search: false,
                    track: Some(track_answer(state, lines.get(1..).unwrap_or_default())),
                })
            }
            _ => {
                osascript(script(app, verb), None)?;
                Ok(Outcome::DONE)
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::time::{Duration, Instant};
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
    };
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// Whether a media session's source app is the wanted app (its AppUserModelID or exe name).
    fn is_app(app: App, source: &str) -> bool {
        let id = source.to_ascii_lowercase();
        match app {
            App::Spotify => id.contains("spotify"),
            App::Music => id.contains("applemusic") || id.contains("itunes"),
        }
    }

    /// Waits for a WinRT operation without the `windows-future` crate's trait: polls its status
    /// (0 is `Started`) and reads the result.
    macro_rules! wait {
        ($op:expr) => {{
            let op = $op;
            let started = Instant::now();
            loop {
                match op.Status() {
                    Ok(status) if status.0 == 0 => {
                        if started.elapsed() > TIMEOUT {
                            break Err(Refusal::new(
                                "timed_out",
                                "the media session did not answer in 5 seconds",
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(_) => {
                        break op.GetResults().map_err(|_| {
                            Refusal::new("act_failed", "the media session refused the command")
                        });
                    }
                    Err(_) => {
                        break Err(Refusal::new(
                            "act_failed",
                            "the media session could not be read",
                        ));
                    }
                }
            }
        }};
    }

    fn session_for(app: App) -> Result<Session> {
        let unavailable =
            || Refusal::new("act_failed", "the system media session is not available");
        let manager: Manager = wait!(Manager::RequestAsync().map_err(|_| unavailable())?)?;
        let sessions = manager.GetSessions().map_err(|_| unavailable())?;
        let count = sessions.Size().map_err(|_| unavailable())?;
        for index in 0..count {
            let Ok(session) = sessions.GetAt(index) else {
                continue;
            };
            let source = session
                .SourceAppUserModelId()
                .map(|id| id.to_string())
                .unwrap_or_default();
            if is_app(app, &source) {
                return Ok(session);
            }
        }
        Err(Refusal::new(
            "not_running",
            "no media session from that app is open; open it first with apps.launch and start something",
        ))
    }

    fn transport(app: App, verb: Verb) -> Result<()> {
        let session = session_for(app)?;
        let accepted: bool = match verb {
            Verb::Play => wait!(session.TryPlayAsync().map_err(|_| unavailable())?)?,
            Verb::Pause => wait!(session.TryPauseAsync().map_err(|_| unavailable())?)?,
            Verb::Next => wait!(session.TrySkipNextAsync().map_err(|_| unavailable())?)?,
            Verb::Previous => wait!(session.TrySkipPreviousAsync().map_err(|_| unavailable())?)?,
            Verb::SearchPlay | Verb::NowPlaying => return Err(unavailable()),
        };
        if accepted {
            Ok(())
        } else {
            Err(Refusal::new(
                "act_failed",
                "the app did not accept the command",
            ))
        }
    }

    fn unavailable() -> Refusal {
        Refusal::new("act_failed", "the system media session is not available")
    }

    /// The session's playback status and its media properties (title, artist, album).
    fn now_playing(app: App) -> Result<Value> {
        let session = session_for(app)?;
        let info = session.GetPlaybackInfo().map_err(|_| unavailable())?;
        // WinRT GlobalSystemMediaTransportControlsSessionPlaybackStatus: 3 Stopped, 4 Playing, 5 Paused.
        let state = match info.PlaybackStatus().map_err(|_| unavailable())?.0 {
            3 => "stopped",
            4 => "playing",
            5 => "paused",
            _ => "unknown",
        };
        let props = wait!(
            session
                .TryGetMediaPropertiesAsync()
                .map_err(|_| unavailable())?
        )?;
        let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
        let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
        let album = props
            .AlbumTitle()
            .map(|s| s.to_string())
            .unwrap_or_default();
        Ok(track_answer(
            state,
            &[title.as_str(), artist.as_str(), album.as_str()],
        ))
    }

    pub fn run(app: App, verb: Verb, query: Option<&str>) -> Result<Outcome> {
        if verb == Verb::SearchPlay {
            if app != App::Spotify {
                return Err(Refusal::new(
                    "unsupported_verb",
                    "search_play on Windows is for Spotify only",
                ));
            }
            let encoded = encode_query(query.unwrap_or_default());
            rightkit_child_process::open_url(&format!("spotify:search:{encoded}"))
                .map_err(|_| Refusal::new("spawn_failed", "the search could not be opened"))?;
            return Ok(Outcome {
                played: false,
                opened_search: true,
                track: None,
            });
        }
        // WinRT wants an initialised thread; a fresh one per call leaves the caller's alone.
        let track = std::thread::spawn(move || {
            let initialised = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
            let result = if verb == Verb::NowPlaying {
                now_playing(app).map(Some)
            } else {
                transport(app, verb).map(|()| None)
            };
            if initialised {
                unsafe { CoUninitialize() };
            }
            result
        })
        .join()
        .map_err(|_| Refusal::new("act_failed", "the media call failed"))??;
        Ok(Outcome {
            played: false,
            opened_search: false,
            track,
        })
    }
}
