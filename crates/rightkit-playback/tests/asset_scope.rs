//! Native source authorisation uses Tauri's actual scope, including Windows verbatim paths.
#![cfg(feature = "tauri")]

use rightkit_playback::tauri_commands::{authorised_media_source_id, PlaybackHost, PlaybackState};
use rightkit_playback::{PlaybackError, PlaybackSource, SourceId, SourceRegistry};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::Manager;

struct Host(Arc<Mutex<SourceRegistry>>);

impl PlaybackHost for Host {
    fn source(&self, _: &str) -> Result<PlaybackSource, String> {
        unreachable!("source ids are resolved through the shared registry")
    }
    fn cache_dir(&self) -> Result<PathBuf, String> {
        unreachable!()
    }
    fn ffmpeg(&self) -> Result<PathBuf, String> {
        unreachable!()
    }
    fn allow_asset(&self, _: &Path) -> Result<(), String> {
        unreachable!()
    }
    fn registry(&self) -> Option<Arc<Mutex<SourceRegistry>>> {
        Some(Arc::clone(&self.0))
    }
}

struct Fixture {
    app: tauri::App<tauri::test::MockRuntime>,
    scope: tauri::scope::fs::Scope,
    registry: Arc<Mutex<SourceRegistry>>,
    scratch: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let app = tauri::test::mock_app();
        let scope = tauri::scope::fs::Scope::new(&app, &Default::default()).unwrap();
        let registry = Arc::new(Mutex::new(SourceRegistry::new()));
        let state = PlaybackState::new(Arc::new(Host(Arc::clone(&registry))));
        state.set_asset_scope(scope.clone());
        app.manage(state);
        Self {
            app,
            scope,
            registry,
            scratch: tempfile::tempdir().unwrap(),
        }
    }

    fn id(&self, path: &Path) -> Result<String, String> {
        authorised_media_source_id(path.to_str().unwrap().to_owned(), self.app.state())
    }

    fn file(&self, relative: &str) -> PathBuf {
        let path = self.scratch.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"non-empty media").unwrap();
        path
    }
}

#[test]
fn command_tracks_recursive_grants_and_forbids_through_shared_registry() {
    let f = Fixture::new();
    let clip = f.file("project/nested/clip.mp4");
    let outside = f.file("project-evil/clip.mp4");
    assert!(f
        .id(&clip)
        .unwrap_err()
        .starts_with("PLAYBACK_NOT_ALLOWED:"));
    assert!(f.registry.lock().unwrap().is_empty());
    f.scope
        .allow_directory(f.scratch.path().join("project"), true)
        .unwrap();
    let id = SourceId::parse(&f.id(&clip).unwrap()).unwrap();
    assert_eq!(
        f.registry.lock().unwrap().resolve(&id).unwrap(),
        clip.canonicalize().unwrap()
    );
    assert!(f
        .id(&outside)
        .unwrap_err()
        .starts_with("PLAYBACK_NOT_ALLOWED:"));

    f.scope
        .forbid_directory(f.scratch.path().join("project/nested"), true)
        .unwrap();
    assert_eq!(
        f.registry.lock().unwrap().resolve(&id),
        Err(PlaybackError::NotAllowed)
    );
    assert!(f
        .id(&clip)
        .unwrap_err()
        .starts_with("PLAYBACK_NOT_ALLOWED:"));
}

#[test]
fn exact_file_grant_does_not_admit_siblings_or_directories() {
    let f = Fixture::new();
    let clip = f.file("project/clip.mp4");
    let sibling = f.file("project/sibling.mp4");
    f.scope.allow_file(&clip).unwrap();
    assert!(f.id(&clip).is_ok());
    assert!(f.id(&sibling).is_err());
    assert!(f.id(clip.parent().unwrap()).is_err());
    assert!(f.id(&f.scratch.path().join("missing.mp4")).is_err());
    let empty = f.file("empty.mp4");
    fs::write(&empty, b"").unwrap();
    f.scope.allow_file(&empty).unwrap();
    assert!(f.id(&empty).unwrap_err().starts_with("PLAYBACK_CORRUPT:"));
    assert_eq!(f.registry.lock().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn symlink_inside_granted_directory_cannot_admit_outside_file() {
    let f = Fixture::new();
    let outside = f.file("secret/clip.mp4");
    let root = f.scratch.path().join("project");
    fs::create_dir(&root).unwrap();
    let link = root.join("clip.mp4");
    std::os::unix::fs::symlink(outside, &link).unwrap();
    f.scope.allow_directory(&root, true).unwrap();
    assert!(f
        .id(&link)
        .unwrap_err()
        .starts_with("PLAYBACK_NOT_ALLOWED:"));
    assert!(f.registry.lock().unwrap().is_empty());
}

#[cfg(windows)]
#[test]
fn windows_plain_and_verbatim_paths_share_id_and_forbidden_precedence() {
    // Exercise both spellings when granting and forbidding: never strip a prefix or OR
    // separate scope decisions, which could bypass a forbid on the other spelling.
    for grant_verbatim in [false, true] {
        let f = Fixture::new();
        let plain = f.file("Project [1]/nested/字幕 clip.mp4");
        let verbatim = plain.canonicalize().unwrap();
        assert!(
            matches!(verbatim.components().next(), Some(std::path::Component::Prefix(p))
            if matches!(p.kind(), std::path::Prefix::VerbatimDisk(_)))
        );
        let root = f.scratch.path().join("Project [1]");
        let grant = if grant_verbatim {
            root.canonicalize().unwrap()
        } else {
            root
        };
        f.scope.allow_directory(grant, true).unwrap();
        let id = f.id(&plain).unwrap();
        assert_eq!(f.id(&verbatim).unwrap(), id);
        assert_eq!(f.registry.lock().unwrap().len(), 1);
        let forbid = if grant_verbatim { &plain } else { &verbatim };
        f.scope.forbid_file(forbid).unwrap();
        assert!(f
            .id(&plain)
            .unwrap_err()
            .starts_with("PLAYBACK_NOT_ALLOWED:"));
        assert!(f
            .id(&verbatim)
            .unwrap_err()
            .starts_with("PLAYBACK_NOT_ALLOWED:"));
        assert_eq!(
            f.registry
                .lock()
                .unwrap()
                .resolve(&SourceId::parse(&id).unwrap()),
            Err(PlaybackError::NotAllowed)
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_long_verbatim_path_remains_usable_without_prefix_stripping() {
    let f = Fixture::new();
    let root = f.scratch.path().canonicalize().unwrap();
    let nested = root.join("x".repeat(120)).join("y".repeat(120));
    fs::create_dir_all(&nested).unwrap();
    let clip = nested.join("clip.mp4");
    fs::write(&clip, b"media").unwrap();
    assert!(clip.as_os_str().len() > 260);
    f.scope.allow_directory(&root, true).unwrap();
    let id = SourceId::parse(&f.id(&clip).unwrap()).unwrap();
    assert_eq!(
        f.registry.lock().unwrap().resolve(&id).unwrap(),
        clip.canonicalize().unwrap()
    );
}
