//! Stage-1 job classifier.
//!
//! Pure URL / local-file-path parsing with **no network calls**.
//! Given an arbitrary string (URL or file path), [`classify`] returns a
//! [`JobKind`] that a media pipeline uses to decide which download
//! strategy and resolver to invoke.
//!
//! # Stage-2 resolution
//!
//! After an [`InstagramPostMaybe`](JobKind::InstagramPostMaybe) is returned
//! the caller should probe yt-dlp with `--dump-json`; if the output contains
//! `"No video formats"` the post is a still-image carousel, otherwise it has
//! video and routes to the audio pipeline (see `queue.rs`).

use url::Url;

mod media_types;
mod urls;

pub use media_types::{
    is_audio_ext, is_document_ext, is_image_ext, is_media_ext, is_video_container_ext,
    is_video_ext, media_types_json, AUDIO_EXTS, DOCUMENT_EXTS, IMAGE_EXTS, VIDEO_CONTAINER_EXTS,
    VIDEO_EXTS,
};
pub use urls::{host, instagram_shortcode, is_blocked_ip, is_public_host};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The kind of media job the app will process.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum JobKind {
    /// `instagram.com` URL whose path contains `/reel/` or `/reels/`.
    InstagramReel,

    /// `instagram.com` URL whose path contains `/p/` — could be a short
    /// video *or* a carousel. The queue probes yt-dlp first; posts without
    /// video formats fall back to carousel capture.
    InstagramPostMaybe,

    /// Any `youtube.com`, `youtu.be`, or `youtube.com/shorts` URL.
    YouTube,

    /// Any `tiktok.com` URL, including `vm.tiktok.com` / `vt.tiktok.com`
    /// short-links.
    TikTok,

    /// `threads.com` or `threads.net` URL.
    Threads,

    /// `facebook.com` or `fb.watch` URL.
    Facebook,

    /// A hosted-video URL on a site yt-dlp can extract (X/Twitter, Vimeo,
    /// Twitch, Reddit, LinkedIn, Loom, Dailymotion, Streamable, SoundCloud…).
    /// Routed to the same audio pipeline as YouTube/TikTok.
    VideoUrl,

    /// A direct media-file URL (path ends in `.mp3`, `.m4a`, `.mp4`, …) — e.g.
    /// a podcast episode download link or a CDN-hosted clip. yt-dlp downloads
    /// it directly, then the audio pipeline transcribes it.
    MediaUrl,

    /// Any other `http(s)` web page — extracted to readable text via the
    /// headless browser (the universal "paste a URL → text" path).
    Article,

    /// A local file path (recognised by a known media extension, a
    /// `file://` scheme, or a path-separator character with no recognised
    /// domain).
    LocalFile,

    /// None of the above; the caller should surface an error to the user.
    Unknown,
}

// ---------------------------------------------------------------------------
// Stage-1 classifier (pure)
// ---------------------------------------------------------------------------

/// Classify `input` as a [`JobKind`] **without making any network request**.
///
/// `input` may be:
/// - An absolute URL (`https://…`, `http://…`, `file://…`)
/// - A bare local path (`/home/user/video.mp4`, `C:\clips\vid.mov`,
///   `./relative.mp4`)
///
/// The function strips `www.` and `m.` sub-domain prefixes before host
/// matching, ignores query strings and fragments, and is case-insensitive
/// on the host.
pub fn classify(input: &str) -> JobKind {
    let trimmed = input.trim();

    // -----------------------------------------------------------------------
    // 0. Fast-exit for Windows absolute paths (e.g. "C:\…" or "D:/…").
    //    url::Url::parse treats "C:" as a scheme and would fall through to
    //    Unknown, so we intercept before URL parsing.
    // -----------------------------------------------------------------------
    if is_windows_absolute_path(trimmed) {
        return JobKind::LocalFile;
    }

    // -----------------------------------------------------------------------
    // 1. Try to parse as a URL first.
    // -----------------------------------------------------------------------
    if let Ok(url) = Url::parse(trimmed) {
        let scheme = url.scheme();

        // file:// URLs → local file regardless of extension
        if scheme == "file" {
            return JobKind::LocalFile;
        }

        if let Some(host_str) = url.host_str() {
            if !is_public_host(host_str) {
                return JobKind::Unknown;
            }
            let host = normalise_host(host_str);
            let path = url.path().to_lowercase();

            // Instagram -------------------------------------------------------
            if host == "instagram.com" {
                if path_contains_segment(&path, "reel") || path_contains_segment(&path, "reels") {
                    return JobKind::InstagramReel;
                }
                if path_contains_segment(&path, "p") {
                    return JobKind::InstagramPostMaybe;
                }
                // instagram.com root or unrecognised path — still Instagram
                // but unknown intent; fall through to Unknown.
                return JobKind::Unknown;
            }

            // YouTube ---------------------------------------------------------
            if host == "youtube.com" || host == "youtu.be" {
                return JobKind::YouTube;
            }

            // TikTok ----------------------------------------------------------
            // `vm.` and `vt.` are TikTok's share-sheet shortlink hosts — both
            // 302 to a canonical /@user/{video,photo}/id URL. Match them
            // explicitly rather than any `*.tiktok.com` subdomain so
            // newsroom/developers/etc. pages still classify as articles.
            if host == "tiktok.com" || host == "vm.tiktok.com" || host == "vt.tiktok.com" {
                return JobKind::TikTok;
            }

            // Threads ---------------------------------------------------------
            if host == "threads.com" || host == "threads.net" {
                return JobKind::Threads;
            }

            // Facebook --------------------------------------------------------
            if host == "facebook.com" || host == "fb.watch" {
                return JobKind::Facebook;
            }

            // Direct media-file URL (podcast episode link, CDN clip) ---------
            // Checked BEFORE the hosted-video list: a path ending in .mp3/.mp4
            // is unambiguous whatever the host, and some hosts appear in both
            // sets (traffic.libsyn.com serves direct episode files under a
            // domain that also hosts show pages).
            if has_media_ext(&path) {
                return JobKind::MediaUrl;
            }

            // Other hosted video (yt-dlp handles extraction) -----------------
            if is_video_host(&host) {
                return JobKind::VideoUrl;
            }

            // Any other regular web page → extract readable article text.
            if scheme == "http" || scheme == "https" {
                return JobKind::Article;
            }
        }

        // A parseable URL with no host or a non-web scheme → Unknown
        return JobKind::Unknown;
    }

    // -----------------------------------------------------------------------
    // 2. Not a valid URL — treat as a local path.
    //    Recognise by: a known media extension OR a path separator character.
    // -----------------------------------------------------------------------
    if looks_like_local_path(trimmed) {
        return JobKind::LocalFile;
    }

    JobKind::Unknown
}

// ---------------------------------------------------------------------------
// Helpers (private)
// ---------------------------------------------------------------------------

/// Strip `www.` and `m.` prefixes and lowercase the host.
fn normalise_host(host: &str) -> String {
    let h = host.to_lowercase();
    let h = h.strip_prefix("www.").unwrap_or(&h);
    let h = h.strip_prefix("m.").unwrap_or(h);
    h.to_owned()
}

/// Return true if `path` (already lowercased) contains `segment` as a
/// complete path component (i.e. surrounded by `/` characters or at
/// start/end of path).
fn path_contains_segment(path: &str, segment: &str) -> bool {
    // Split on '/' and look for an exact match on any component.
    path.split('/').any(|c| c == segment)
}

/// Return true for Windows absolute paths like `C:\…` or `D:/…`.
///
/// A Windows absolute path starts with a single ASCII letter followed by
/// `:` and then a backslash or forward slash.
fn is_windows_absolute_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Heuristic: is this string a local file path rather than a URL?
///
/// Returns `true` when any of these hold:
/// - The string contains a path separator (`/` or `\`).
/// - The string has one of the recognised media extensions.
fn looks_like_local_path(s: &str) -> bool {
    if s.contains('/') || s.contains('\\') {
        return true;
    }
    media_types::last_extension(s).is_some_and(is_media_ext)
}

/// Known hosted-video / hosted-audio sites yt-dlp can extract. Matched as the
/// host itself or any sub-domain of it (`clips.twitch.tv`, `v.redd.it`, …).
/// Spotify is deliberately excluded — its audio is DRM-protected and yt-dlp
/// cannot fetch it.
///
/// A host listed here routes to the media pipeline first. When the URL turns
/// out to be an ordinary page on that host (an artist's merch page, a show's
/// index) yt-dlp reports no media and the engine falls back to article
/// extraction rather than erroring — see
/// `crates/engine/src/extraction.rs::process` and
/// `extraction_audio::should_fall_back_to_article`. That fallback is what
/// makes adding a host here safe: the cost of a wrong guess is one extra
/// probe, not a failed job.
fn is_video_host(host: &str) -> bool {
    const VIDEO_HOSTS: &[&str] = &[
        "twitter.com",
        "x.com",
        "vimeo.com",
        "twitch.tv",
        "reddit.com",
        "redd.it",
        "linkedin.com",
        "loom.com",
        "dailymotion.com",
        "dai.ly",
        "streamable.com",
        "soundcloud.com",
        "mixcloud.com",
        "bandcamp.com",
        "rumble.com",
        "odysee.com",
        "nebula.tv",
        "bitchute.com",
        "podbean.com",
        "buzzsprout.com",
        "spreaker.com",
        "libsyn.com",
        "megaphone.fm",
        "castbox.fm",
        "audioboom.com",
    ];
    VIDEO_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// True when a URL path points directly at a media file (podcast episode
/// download link, CDN-hosted clip). `path` must already be lowercased.
fn has_media_ext(path: &str) -> bool {
    media_types::last_extension(path).is_some_and(is_media_ext)
}

// ---------------------------------------------------------------------------
// Profile URLs (#248 batch image download)
// ---------------------------------------------------------------------------

/// A creator-profile page (a listing of posts), not a single post. Kept apart
/// from [`JobKind`] because a profile is never queued as one job: the engine
/// enumerates it and queues one job per post.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ProfileKind {
    Instagram,
    TikTok,
    X,
}

/// Path segments that look like a profile name but are site sections.
const INSTAGRAM_RESERVED: &[&str] = &[
    "p",
    "reel",
    "reels",
    "tv",
    "stories",
    "explore",
    "accounts",
    "direct",
    "about",
    "legal",
    "privacy",
    "developer",
    "web",
    "press",
    "api",
    "challenge",
    "emailsignup",
    "oauth",
    "s",
    "directory",
    "locations",
    "nametag",
    "ar",
    "lite",
];
const X_RESERVED: &[&str] = &[
    "home",
    "i",
    "search",
    "explore",
    "settings",
    "messages",
    "notifications",
    "compose",
    "intent",
    "share",
    "hashtag",
    "login",
    "logout",
    "signup",
    "tos",
    "privacy",
    "about",
    "jobs",
    "download",
    "account",
    "status",
    "who_to_follow",
    "lists",
    "bookmarks",
];

/// Classify `input` as a profile URL, or `None` when it is anything else
/// (a post, a reel, an article, a path). Pure: no network.
///
/// Recognised: `instagram.com/<user>[/[reels|tagged]]`, `tiktok.com/@<user>`,
/// and `x.com|twitter.com/<user>[/[media|with_replies]]`.
pub fn classify_profile(input: &str) -> Option<ProfileKind> {
    let url = Url::parse(input.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = normalise_host(url.host_str()?);
    let path = url.path().to_lowercase();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let name_ok = |s: &str, extra: &[char]| {
        !s.is_empty()
            && s.len() <= 40
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || extra.contains(&c))
    };
    match host.as_str() {
        "instagram.com" => {
            let first = *segs.first()?;
            if INSTAGRAM_RESERVED.contains(&first) || !name_ok(first, &['.']) {
                return None;
            }
            match segs.len() {
                1 => Some(ProfileKind::Instagram),
                2 if matches!(segs[1], "reels" | "tagged") => Some(ProfileKind::Instagram),
                _ => None,
            }
        }
        "tiktok.com" => {
            let first = *segs.first()?;
            let user = first.strip_prefix('@')?;
            (segs.len() == 1 && name_ok(user, &['.'])).then_some(ProfileKind::TikTok)
        }
        "x.com" | "twitter.com" => {
            let first = *segs.first()?;
            if X_RESERVED.contains(&first) || !name_ok(first, &[]) {
                return None;
            }
            match segs.len() {
                1 => Some(ProfileKind::X),
                2 if matches!(segs[1], "media" | "with_replies" | "highlights") => {
                    Some(ProfileKind::X)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn instagram_profiles_are_recognised() {
        for url in [
            "https://www.instagram.com/some.user/",
            "https://instagram.com/some_user",
            "https://m.instagram.com/some.user/?hl=en",
            "https://www.instagram.com/some.user/reels/",
            "https://www.instagram.com/Some.User/#top",
        ] {
            assert_eq!(classify_profile(url), Some(ProfileKind::Instagram), "{url}");
        }
    }

    #[test]
    fn instagram_posts_sections_and_root_are_not_profiles() {
        for url in [
            "https://www.instagram.com/p/ABC123/",
            "https://www.instagram.com/some.user/p/ABC123/",
            "https://www.instagram.com/reel/ABC123/",
            "https://www.instagram.com/explore/",
            "https://www.instagram.com/accounts/login/",
            "https://www.instagram.com/",
            "https://www.instagram.com/stories/some.user/1/",
        ] {
            assert_eq!(classify_profile(url), None, "{url}");
        }
    }

    #[test]
    fn tiktok_and_x_profiles_are_recognised() {
        assert_eq!(
            classify_profile("https://www.tiktok.com/@some.user"),
            Some(ProfileKind::TikTok)
        );
        assert_eq!(
            classify_profile("https://x.com/someuser"),
            Some(ProfileKind::X)
        );
        assert_eq!(
            classify_profile("https://twitter.com/someuser/media"),
            Some(ProfileKind::X)
        );
    }

    #[test]
    fn tiktok_and_x_posts_are_not_profiles() {
        for url in [
            "https://www.tiktok.com/@some.user/video/123",
            "https://www.tiktok.com/discover",
            "https://x.com/someuser/status/123",
            "https://x.com/home",
            "https://x.com/i/status/1",
        ] {
            assert_eq!(classify_profile(url), None, "{url}");
        }
    }

    #[test]
    fn non_urls_and_other_hosts_are_not_profiles() {
        for input in [
            "",
            r"C:\clips\a.mp4",
            "/tmp/a.mp4",
            "https://example.com/user",
            "ftp://instagram.com/user",
        ] {
            assert_eq!(classify_profile(input), None, "{input}");
        }
    }

    #[test]
    fn classify_is_unchanged_for_profiles() {
        // The post classifier stays as it was; profiles are a separate query.
        assert_eq!(
            classify("https://www.instagram.com/some.user/"),
            JobKind::Unknown
        );
    }
}

// ---------------------------------------------------------------------------
// Playlists and channels (#251)
// ---------------------------------------------------------------------------

/// A multi-video listing yt-dlp can enumerate. Like [`ProfileKind`] it is never
/// queued as one job: the engine lists it and queues one job per entry.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum PlaylistKind {
    /// `youtube.com/playlist?list=...`
    YouTubePlaylist,
    /// `youtube.com/@handle`, `/channel/<id>`, `/c/<name>`, `/user/<name>`,
    /// optionally followed by a `videos`, `shorts` or `streams` tab.
    YouTubeChannel,
    /// Vimeo showcase, album or channel.
    Vimeo,
    /// SoundCloud `/<user>/sets/<name>`.
    SoundCloudSet,
    /// Dailymotion `/playlist/<id>`.
    Dailymotion,
}

/// Classify `input` as a playlist or channel URL, or `None` for anything else
/// (a single video, a watch link that merely carries `list=`, an article).
/// Pure: no network. A `watch?v=..&list=..` link stays a single video.
pub fn classify_playlist(input: &str) -> Option<PlaylistKind> {
    let trimmed = input.trim();
    let url = Url::parse(trimmed).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    // Same host safety gate as `classify`: never enumerate local or private hosts.
    if !matches!(classify(trimmed), JobKind::YouTube | JobKind::VideoUrl) {
        return None;
    }
    let host = normalise_host(url.host_str()?);
    let path = url.path().to_lowercase();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let tab_ok = |rest: &[&str]| match rest {
        [] => true,
        [tab] => matches!(*tab, "videos" | "shorts" | "streams"),
        _ => false,
    };
    match host.as_str() {
        "youtube.com" => {
            if segs.first() == Some(&"playlist") && segs.len() == 1 {
                let has_list = url
                    .query_pairs()
                    .any(|(k, v)| k == "list" && !v.trim().is_empty());
                return has_list.then_some(PlaylistKind::YouTubePlaylist);
            }
            let first = *segs.first()?;
            if first.starts_with('@') && first.len() > 1 && tab_ok(&segs[1..]) {
                return Some(PlaylistKind::YouTubeChannel);
            }
            if matches!(first, "channel" | "c" | "user") && segs.len() >= 2 && tab_ok(&segs[2..]) {
                return Some(PlaylistKind::YouTubeChannel);
            }
            None
        }
        "vimeo.com" => match segs.as_slice() {
            ["showcase" | "album" | "channels", name] if !name.is_empty() => {
                Some(PlaylistKind::Vimeo)
            }
            _ => None,
        },
        "soundcloud.com" => match segs.as_slice() {
            [user, "sets", name] if !user.is_empty() && !name.is_empty() => {
                Some(PlaylistKind::SoundCloudSet)
            }
            _ => None,
        },
        "dailymotion.com" => match segs.as_slice() {
            ["playlist", id] if !id.is_empty() => Some(PlaylistKind::Dailymotion),
            _ => None,
        },
        _ => None,
    }
}

/// URL actually enumerated: a bare YouTube channel gets its `/videos` tab so
/// yt-dlp lists videos, not the channel's tab list.
pub fn playlist_listing_url(input: &str) -> String {
    let trimmed = input.trim();
    if classify_playlist(trimmed) != Some(PlaylistKind::YouTubeChannel) {
        return trimmed.to_string();
    }
    let Ok(mut url) = Url::parse(trimmed) else {
        return trimmed.to_string();
    };
    let segs: Vec<String> = url
        .path()
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let base = if segs[0].starts_with('@') { 1 } else { 2 };
    if segs.len() == base {
        let path = format!("/{}/videos", segs.join("/"));
        url.set_path(&path);
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod playlist_tests {
    use super::*;

    #[test]
    fn youtube_playlists_and_channels_are_detected() {
        assert_eq!(
            classify_playlist("https://www.youtube.com/playlist?list=PL123"),
            Some(PlaylistKind::YouTubePlaylist)
        );
        for url in [
            "https://www.youtube.com/@someone",
            "https://youtube.com/@someone/videos",
            "https://m.youtube.com/channel/UCabc123",
            "https://www.youtube.com/c/Name/shorts",
            "https://www.youtube.com/user/Name",
        ] {
            assert_eq!(
                classify_playlist(url),
                Some(PlaylistKind::YouTubeChannel),
                "{url}"
            );
        }
    }

    #[test]
    fn single_videos_and_odd_pages_are_not_playlists() {
        for url in [
            "https://www.youtube.com/watch?v=abc&list=PL123",
            "https://youtu.be/abc?list=PL123",
            "https://www.youtube.com/shorts/abc",
            "https://www.youtube.com/playlist",
            "https://www.youtube.com/playlist?list=",
            "https://www.youtube.com/@someone/community",
            "https://www.youtube.com/",
            "https://example.com/playlist?list=1",
            "https://localhost/playlist?list=1",
            r"C:\clips\a.mp4",
            "ftp://www.youtube.com/@someone",
        ] {
            assert_eq!(classify_playlist(url), None, "{url}");
        }
    }

    #[test]
    fn other_supported_hosts_are_detected() {
        assert_eq!(
            classify_playlist("https://vimeo.com/showcase/123456"),
            Some(PlaylistKind::Vimeo)
        );
        assert_eq!(
            classify_playlist("https://soundcloud.com/artist/sets/album"),
            Some(PlaylistKind::SoundCloudSet)
        );
        assert_eq!(
            classify_playlist("https://www.dailymotion.com/playlist/x7abc"),
            Some(PlaylistKind::Dailymotion)
        );
        assert_eq!(classify_playlist("https://vimeo.com/123456"), None);
        assert_eq!(
            classify_playlist("https://soundcloud.com/artist/track"),
            None
        );
    }

    #[test]
    fn bare_channels_list_their_videos_tab() {
        assert_eq!(
            playlist_listing_url("https://www.youtube.com/@someone"),
            "https://www.youtube.com/@someone/videos"
        );
        assert_eq!(
            playlist_listing_url("https://www.youtube.com/channel/UCabc/"),
            "https://www.youtube.com/channel/UCabc/videos"
        );
        assert_eq!(
            playlist_listing_url("https://www.youtube.com/@someone/shorts"),
            "https://www.youtube.com/@someone/shorts"
        );
        assert_eq!(
            playlist_listing_url("https://www.youtube.com/playlist?list=PL1"),
            "https://www.youtube.com/playlist?list=PL1"
        );
    }
}

#[cfg(test)]
mod classify_shared_table_tests {
    use super::*;

    #[test]
    fn direct_media_urls_use_the_shared_extension_table() {
        for url in [
            "https://cdn.example.com/a.ogg",
            "https://cdn.example.com/a.OPUS",
            "https://cdn.example.com/a.avi",
            "https://cdn.example.com/a.wma",
        ] {
            assert_eq!(classify(url), JobKind::MediaUrl, "{url}");
        }
        assert_eq!(classify("https://example.com/a.png"), JobKind::Article);
    }

    #[test]
    fn bare_media_filenames_are_local_files() {
        for name in ["clip.ogg", "talk.AVI", "song.opus"] {
            assert_eq!(classify(name), JobKind::LocalFile, "{name}");
        }
        assert_eq!(classify("notes.txt"), JobKind::Unknown);
    }

    #[test]
    fn private_and_local_hosts_stay_unknown() {
        for url in [
            "http://localhost/a",
            "http://127.0.0.1/a",
            "http://192.168.1.5/a.mp3",
            "http://100.64.0.1/a",
            "http://[::1]/a",
        ] {
            assert_eq!(classify(url), JobKind::Unknown, "{url}");
        }
    }
}
