//! Host-authorised sources: the opt-in hook, canonicalisation, and re-checking at use.
//! Real files in a real temp directory; no FFmpeg needed.

use rightkit_playback::{allowed_roots, PlaybackError, SourceRegistry};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "rightkit-playback-authorised-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn registry_allowing(root: &Path) -> SourceRegistry {
    let mut registry = SourceRegistry::new();
    registry.set_authoriser(allowed_roots([root.to_path_buf()]));
    registry
}

#[test]
fn file_in_a_nested_directory_of_an_allowed_root_is_registered() {
    let s = Scratch::new("nested");
    let clip = s.file("project/a/b/clip.mp4", b"non-empty media");
    let mut registry = registry_allowing(&s.dir("project"));

    let id = registry.resolve_or_register(&clip).unwrap();
    assert_eq!(registry.resolve(&id).unwrap(), clip.canonicalize().unwrap());
    assert_eq!(
        registry.resolve_or_register(&clip).unwrap(),
        id,
        "stable id"
    );
    assert_eq!(registry.len(), 1);
}

#[test]
fn sibling_outside_the_root_is_refused_even_with_a_shared_name_prefix() {
    let s = Scratch::new("sibling");
    let outside = s.file("project-evil/clip.mp4", b"non-empty media");
    let other = s.file("other/clip.mp4", b"non-empty media");
    let mut registry = registry_allowing(&s.dir("project"));

    for path in [&outside, &other] {
        assert_eq!(
            registry.resolve_or_register(path),
            Err(PlaybackError::NotAllowed)
        );
    }
    assert!(registry.is_empty());
}

#[test]
fn dot_dot_escape_is_refused() {
    let s = Scratch::new("dotdot");
    s.file("secret/clip.mp4", b"non-empty media");
    let root = s.dir("project");
    let mut registry = registry_allowing(&root);

    let escape = root.join("..").join("secret").join("clip.mp4");
    assert!(escape.starts_with(&root), "textually inside the root");
    assert_eq!(
        registry.resolve_or_register(&escape),
        Err(PlaybackError::NotAllowed)
    );
    assert!(registry.is_empty());
}

#[cfg(unix)]
#[test]
fn symlink_inside_the_root_pointing_outside_is_refused() {
    let s = Scratch::new("symlink");
    let outside = s.file("secret/clip.mp4", b"non-empty media");
    let root = s.dir("project");
    let link = root.join("link.mp4");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let dir_link = root.join("dirlink");
    std::os::unix::fs::symlink(outside.parent().unwrap(), &dir_link).unwrap();
    let mut registry = registry_allowing(&root);

    assert_eq!(
        registry.resolve_or_register(&link),
        Err(PlaybackError::NotAllowed)
    );
    assert_eq!(
        registry.resolve_or_register(dir_link.join("clip.mp4")),
        Err(PlaybackError::NotAllowed)
    );
    assert!(registry.is_empty());
}

#[cfg(unix)]
#[test]
fn symlink_swapped_in_after_registration_is_refused_on_use() {
    let s = Scratch::new("swap");
    let outside = s.file("secret/clip.mp4", b"secret media bytes");
    let root = s.dir("project");
    let clip = s.file("project/clip.mp4", b"secret media bytes");
    let mut registry = registry_allowing(&root);
    let id = registry.resolve_or_register(&clip).unwrap();

    fs::remove_file(&clip).unwrap();
    std::os::unix::fs::symlink(&outside, &clip).unwrap();
    assert_eq!(registry.resolve(&id), Err(PlaybackError::NotAllowed));
}

#[test]
fn directory_is_refused() {
    let s = Scratch::new("dir");
    let root = s.dir("project");
    let sub = s.dir("project/sub");
    let mut registry = registry_allowing(&root);

    assert!(registry.resolve_or_register(&sub).is_err());
    assert!(registry.resolve_or_register(&root).is_err());
    assert!(registry.is_empty());
}

#[test]
fn missing_file_is_refused() {
    let s = Scratch::new("missing");
    let root = s.dir("project");
    let mut registry = registry_allowing(&root);

    assert!(registry.resolve_or_register(root.join("nope.mp4")).is_err());
    assert!(registry.is_empty());
}

#[test]
fn without_a_hook_nothing_is_registered() {
    let s = Scratch::new("nohook");
    let clip = s.file("project/clip.mp4", b"non-empty media");
    let mut registry = SourceRegistry::new();

    assert!(!registry.has_authoriser());
    assert_eq!(
        registry.resolve_or_register(&clip),
        Err(PlaybackError::NotAllowed)
    );
    assert!(registry.is_empty());
    assert_eq!(registry.id_for_path(&clip), None);
    // The pre-existing contract: a bare path is not a source.
    assert_eq!(
        registry.resolve_ref(clip.to_str().unwrap()),
        Err(PlaybackError::InvalidRequest)
    );
}

#[test]
fn revoking_the_hook_refuses_an_existing_id_on_use() {
    let s = Scratch::new("revoke");
    let clip = s.file("project/clip.mp4", b"non-empty media");
    let allowed = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&allowed);
    let mut registry = SourceRegistry::new();
    registry.set_authoriser(move |_| flag.load(Ordering::SeqCst));

    let id = registry.resolve_or_register(&clip).unwrap();
    assert!(registry.resolve(&id).is_ok());

    allowed.store(false, Ordering::SeqCst);
    assert_eq!(registry.resolve(&id), Err(PlaybackError::NotAllowed));
    assert_eq!(
        registry.resolve_ref(id.as_str()),
        Err(PlaybackError::NotAllowed)
    );
    assert_eq!(
        registry.resolve_or_register(&clip),
        Err(PlaybackError::NotAllowed)
    );

    allowed.store(true, Ordering::SeqCst);
    assert!(registry.resolve(&id).is_ok(), "allowed again, same id");

    registry.clear_authoriser();
    assert_eq!(registry.resolve(&id), Err(PlaybackError::NotAllowed));
}

#[test]
fn explicit_registration_does_not_depend_on_the_hook() {
    let s = Scratch::new("explicit");
    let clip = s.file("elsewhere/clip.mp4", b"non-empty media");
    let mut registry = registry_allowing(&s.dir("project"));
    let id = registry.register(&clip).unwrap();

    registry.clear_authoriser();
    assert!(registry.resolve(&id).is_ok());
}

#[test]
fn a_changed_file_still_fails_closed() {
    let s = Scratch::new("changed");
    let clip = s.file("project/clip.mp4", b"first");
    let mut registry = registry_allowing(&s.dir("project"));
    let id = registry.resolve_or_register(&clip).unwrap();

    fs::write(&clip, b"second, longer").unwrap();
    assert_eq!(registry.resolve(&id), Err(PlaybackError::Missing));
    let fresh = registry.resolve_or_register(&clip).unwrap();
    assert_ne!(fresh, id);
    assert_eq!(registry.len(), 1);
}

#[test]
fn allowed_roots_ignores_missing_roots_and_the_root_itself() {
    let s = Scratch::new("roots");
    let clip = s.file("project/clip.mp4", b"non-empty media");
    let hook = allowed_roots([s.0.join("does-not-exist"), s.0.join("project")]);
    assert!(hook(&clip));
    assert!(!hook(&s.0.join("project")));
    assert!(!hook(&s.0.join("project/missing.mp4")));
}
