// rightkit-playback 0.3.0 consumed by exact registry pin, outside the workspace.
//
// Proves CutRight's media-authorization path (cutright src-tauri/src/lib.rs:89-90: directory
// grants become an allowed_roots authoriser set on the playback state; :214 registers the
// authorised_media_source_id command, exercised here through its PlaybackState method): a host
// authoriser admits a regular file under the granted root; a file outside the grant is refused;
// a directory and an empty file inside the grant are refused; an unresolvable path is refused; a
// registered path is found again by media_source_id; clearing the hook revokes the id it admitted.
// It also drives the live Tauri asset scope (PlaybackState::set_asset_scope) against a tauri::test
// mock app: grants and forbids made on the scope after installation take effect, a forbid beats a
// grant, and on Windows the plain and verbatim spellings of one path share a decision.
// Does not prove: prepare_registered_media_playback or compatibility_copy (both run ffmpeg), or the
// Tauri command wrappers (they need a live tauri State).
// State: temp files in a temp directory, removed on drop. No ffmpeg, no network, no user data.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::scope::fs::Scope;

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
        let dir =
            std::env::temp_dir().join(format!("rk-playback-{tag}-{}-{nanos}", std::process::id()));
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
        state
            .media_source_id(&media)
            .expect("registered path is found again"),
        id
    );

    // Refusals: each one returns a distinct error and registers nothing.
    assert!(
        matches!(
            state.authorised_media_source_id(&outside),
            Err(PlaybackError::NotAllowed)
        ),
        "a file outside the grant is refused"
    );
    assert!(
        matches!(
            state.authorised_media_source_id(root.join("sub")),
            Err(PlaybackError::Empty)
        ),
        "a directory inside the grant is not a source"
    );
    assert!(
        matches!(
            state.authorised_media_source_id(&empty),
            Err(PlaybackError::Empty)
        ),
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
        matches!(
            state.media_source_id(&media),
            Err(PlaybackError::NotAllowed)
        ),
        "clearing the hook revokes the admitted id"
    );
    assert!(
        matches!(
            state.authorised_media_source_id(&media),
            Err(PlaybackError::NotAllowed)
        ),
        "without a hook nothing is authorised"
    );

    asset_scope();

    println!("OK rightkit-playback 0.3.0 (registry consumer, tauri feature, live asset scope, no ffmpeg)");
}

// PlaybackState::set_asset_scope against a real tauri Scope built from a tauri::test mock app.
fn asset_scope() {
    let granted = TempDir::new("scope-granted");
    let other = TempDir::new("scope-other");
    let root = granted.0.join("project");
    fs::create_dir_all(root.join("nested")).expect("create scoped tree");
    let clip = root.join("nested").join("clip.mp4");
    fs::write(&clip, b"registration only, never decoded").expect("write scoped media");
    let sibling = root.join("sibling.mp4");
    fs::write(&sibling, b"registration only, never decoded").expect("write sibling media");
    let outside = other.0.join("outside.mp4");
    fs::write(&outside, b"outside the grant").expect("write outside file");

    let app = tauri::test::mock_app();
    let scope = Scope::new(&app, &Default::default()).expect("fs scope");
    let state = PlaybackState::new(Arc::new(NoFfmpegHost));
    state.set_asset_scope(scope.clone());

    assert!(
        matches!(
            state.authorised_media_source_id(&clip),
            Err(PlaybackError::NotAllowed)
        ),
        "an empty scope admits nothing"
    );
    // Grants made after the scope was installed are seen: the hook shares the live scope.
    scope.allow_directory(&root, true).expect("grant directory");
    let id = state
        .authorised_media_source_id(&clip)
        .expect("a file under the granted directory is admitted");
    assert_eq!(id.as_str().len(), 64, "a source id is 64 hex characters");
    assert!(
        matches!(
            state.authorised_media_source_id(&outside),
            Err(PlaybackError::NotAllowed)
        ),
        "a file outside the granted directory is refused"
    );
    // A forbid wins over the recursive grant and revokes the id it had admitted.
    scope
        .forbid_directory(root.join("nested"), true)
        .expect("forbid directory");
    assert!(
        matches!(
            state.authorised_media_source_id(&clip),
            Err(PlaybackError::NotAllowed)
        ),
        "a forbidden directory beats the recursive grant"
    );
    assert!(
        matches!(state.media_source_id(&clip), Err(PlaybackError::NotAllowed)),
        "the forbid revokes the id the scope admitted"
    );
    state
        .authorised_media_source_id(&sibling)
        .expect("a sibling outside the forbidden directory is still admitted");

    // Windows: the plain and the verbatim (\?\) spelling of one path share one decision.
    #[cfg(windows)]
    {
        let plain = sibling.clone();
        let verbatim = plain.canonicalize().expect("canonical path");
        assert!(
            matches!(
                verbatim.components().next(),
                Some(std::path::Component::Prefix(p))
                    if matches!(p.kind(), std::path::Prefix::VerbatimDisk(_))
            ),
            "canonical Windows paths are verbatim drive paths"
        );
        assert_eq!(
            state
                .authorised_media_source_id(&verbatim)
                .expect("verbatim spelling"),
            state
                .authorised_media_source_id(&plain)
                .expect("plain spelling"),
            "both spellings resolve to one id"
        );
        scope
            .forbid_file(&verbatim)
            .expect("forbid verbatim spelling");
        assert!(
            matches!(
                state.authorised_media_source_id(&plain),
                Err(PlaybackError::NotAllowed)
            ),
            "a forbid on the verbatim spelling refuses the plain spelling"
        );
        assert!(
            matches!(
                state.authorised_media_source_id(&verbatim),
                Err(PlaybackError::NotAllowed)
            ),
            "a forbid on the verbatim spelling refuses itself"
        );
    }

    // Clearing the hook drops the scope: nothing it admitted resolves afterwards.
    state.clear_authoriser();
    assert!(
        matches!(
            state.authorised_media_source_id(&sibling),
            Err(PlaybackError::NotAllowed)
        ),
        "without a hook nothing is authorised"
    );
}
