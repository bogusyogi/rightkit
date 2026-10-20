// rightkit-playback 0.3.0 consumed by exact registry pin, outside the workspace.
//
// Proves CutRight's media-authorization path (cutright src-tauri/src/lib.rs:89-90: directory
// grants become an allowed_roots authoriser set on the playback state; :214 registers the
// authorised_media_source_id command, exercised here through its PlaybackState method): a host
// authoriser admits a regular file under the granted root; a file outside the grant is refused;
// a directory and an empty file inside the grant are refused; an unresolvable path is refused; a
// registered path is found again by media_source_id; clearing the hook revokes the id it admitted.
// Does not prove: prepare_registered_media_playback or compatibility_copy (both run ffmpeg), the
// Tauri command wrappers (they need a live tauri State), or the asset-protocol scope.
// State: temp files in a temp directory, removed on drop. No ffmpeg, no network, no user data.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_playback::tauri_commands::{PlaybackHost, PlaybackState};
use rightkit_playback::{allowed_roots, PlaybackError, PlaybackSource};

// The authorisation path never calls the host, so every host method refuses.
struct NoFfmpegHost;

impl PlaybackHost for NoFfmpegHost {
    fn source(&self, _id: &str) -> Result<PlaybackSource, String> {
        Err("not used by this consumer".into())
    }

    fn cache_dir(&self) -> Result<PathBuf, String> {
        Err("not used by this consumer".into())
    }

    fn ffmpeg(&self) -> Result<PathBuf, String> {
        Err("not used by this consumer".into())
    }

    fn allow_asset(&self, _path: &Path) -> Result<(), String> {
        Err("not used by this consumer".into())
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "rk-playback-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() {
    let granted = TempDir::new("granted");
    let other = TempDir::new("other");
    let root = granted.0.clone();

    let media = root.join("clip.mp4");
    fs::write(&media, b"registration only, never decoded").expect("write media");
    let empty = root.join("empty.mp4");
    fs::write(&empty, b"").expect("write empty file");
    fs::create_dir(root.join("sub")).expect("create subdirectory");
    let outside = other.0.join("outside.mp4");
    fs::write(&outside, b"outside the grant").expect("write outside file");

    // CutRight lib.rs:89-90: a directory grant becomes an allowed_roots authoriser on the state.
    let state = PlaybackState::new(Arc::new(NoFfmpegHost));
    let allowed = allowed_roots(vec![root.clone()]);
    state.set_authoriser(move |path: &Path| allowed(path));

    // lib.rs:214: resolve-or-register for a file the hook admits.
    let id = state
        .authorised_media_source_id(&media)
        .expect("regular file under the granted root");
    assert_eq!(id.as_str().len(), 64, "a source id is 64 hex characters");
    assert_eq!(
        state.media_source_id(&media).expect("registered path is found again"),
        id
    );

    // Refusals: each one returns a distinct error and registers nothing.
    assert!(
        matches!(state.authorised_media_source_id(&outside), Err(PlaybackError::NotAllowed)),
        "a file outside the grant is refused"
    );
    assert!(
        matches!(state.authorised_media_source_id(root.join("sub")), Err(PlaybackError::Empty)),
        "a directory inside the grant is not a source"
    );
    assert!(
        matches!(state.authorised_media_source_id(&empty), Err(PlaybackError::Empty)),
        "an empty file is not a source"
    );
    assert!(
        matches!(
            state.authorised_media_source_id(root.join("missing.mp4")),
            Err(PlaybackError::NotAllowed)
        ),
        "a path that does not resolve is refused without revealing whether it exists"
    );

    // Revocation: clearing the hook stops every id it admitted; nothing resolves afterwards.
    state.clear_authoriser();
    assert!(
        matches!(state.media_source_id(&media), Err(PlaybackError::NotAllowed)),
        "clearing the hook revokes the admitted id"
    );
    assert!(
        matches!(state.authorised_media_source_id(&media), Err(PlaybackError::NotAllowed)),
        "without a hook nothing is authorised"
    );

    println!("OK rightkit-playback 0.3.0 (registry consumer, tauri feature, no ffmpeg)");
}
