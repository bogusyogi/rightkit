//! Registration, revocation and refusal for media sources. Real files in a real temp directory;
//! no FFmpeg needed.

use rightkit_playback::{PlaybackError, SourceId, SourceRegistry};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "rightkit-playback-sources-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn unknown_id() -> SourceId {
    SourceId::parse(&"ab".repeat(32)).unwrap()
}

#[test]
fn registration_is_idempotent_and_resolves_to_the_canonical_path() {
    let dir = Scratch::new("idempotent");
    let clip = dir.file("clip.mp4", b"not really a movie but non-empty");
    let mut registry = SourceRegistry::new();

    let first = registry.register(&clip).unwrap();
    let second = registry.register(&clip).unwrap();
    assert_eq!(first, second, "the same unchanged file keeps its id");
    assert_eq!(registry.len(), 1);

    let resolved = registry.resolve(&first).unwrap();
    assert_eq!(resolved, clip.canonicalize().unwrap().as_path());
    assert_eq!(registry.resolve_ref(first.as_str()).unwrap(), resolved);
    assert_eq!(registry.id_for_path(&clip), Some(first.clone()));

    let text = first.to_string();
    assert_eq!(text.len(), 64);
    assert_eq!(SourceId::parse(&text), Some(first));
}

#[test]
fn distinct_files_get_distinct_ids() {
    let dir = Scratch::new("distinct");
    let a = dir.file("a.mp4", b"aaaa");
    let b = dir.file("b.mp4", b"aaaa");
    let mut registry = SourceRegistry::new();
    let id_a = registry.register(&a).unwrap();
    let id_b = registry.register(&b).unwrap();
    assert_ne!(id_a, id_b, "identity includes the path, not just the bytes");
    assert_eq!(registry.len(), 2);
}

#[test]
fn unregistered_sources_are_refused() {
    let dir = Scratch::new("refused");
    let registered = dir.file("in.mp4", b"registered");
    let unregistered = dir.file("out.mp4", b"exists on disk but was never registered");
    let mut registry = SourceRegistry::new();
    registry.register(&registered).unwrap();

    assert!(matches!(
        registry.resolve(&unknown_id()),
        Err(PlaybackError::InvalidRequest)
    ));
    assert!(matches!(
        registry.resolve_ref(unknown_id().as_str()),
        Err(PlaybackError::InvalidRequest)
    ));
    assert!(matches!(
        registry.resolve_ref(unregistered.to_str().unwrap()),
        Err(PlaybackError::InvalidRequest)
    ));
    assert!(matches!(
        registry.resolve_ref("/definitely/not/a/registered/path.mp4"),
        Err(PlaybackError::InvalidRequest)
    ));
    assert_eq!(registry.id_for_path(&unregistered), None);
    assert!(registry.resolve_ref(registered.to_str().unwrap()).is_ok());
}

#[test]
fn revocation_removes_access_and_keeps_the_file() {
    let dir = Scratch::new("revoke");
    let clip = dir.file("clip.mp4", b"revocable");
    let mut registry = SourceRegistry::new();
    let id = registry.register(&clip).unwrap();

    assert!(registry.revoke(&id), "first revoke removes the source");
    assert!(!registry.revoke(&id), "second revoke finds nothing");
    assert!(registry.is_empty());

    assert!(matches!(
        registry.resolve(&id),
        Err(PlaybackError::InvalidRequest)
    ));
    assert!(matches!(
        registry.resolve_ref(id.as_str()),
        Err(PlaybackError::InvalidRequest)
    ));
    assert!(matches!(
        registry.resolve_ref(clip.to_str().unwrap()),
        Err(PlaybackError::InvalidRequest)
    ));
    assert!(clip.is_file(), "revocation never deletes the media");

    // Registering again issues a usable id.
    let again = registry.register(&clip).unwrap();
    assert_eq!(again, id, "same file, same identity");
    assert!(registry.resolve(&again).is_ok());
}

#[test]
fn revoke_ref_accepts_an_id_or_a_registered_path() {
    let dir = Scratch::new("revoke-ref");
    let by_id = dir.file("one.mp4", b"one");
    let by_path = dir.file("two.mp4", b"two");
    let mut registry = SourceRegistry::new();
    let id_one = registry.register(&by_id).unwrap();
    registry.register(&by_path).unwrap();

    assert!(registry.revoke_ref(id_one.as_str()));
    assert!(registry.revoke_ref(by_path.to_str().unwrap()));
    assert!(!registry.revoke_ref(by_path.to_str().unwrap()));
    assert!(registry.is_empty());
}

#[test]
fn a_changed_file_fails_closed_until_it_is_registered_again() {
    let dir = Scratch::new("changed");
    let clip = dir.file("clip.mp4", b"original bytes");
    let mut registry = SourceRegistry::new();
    let old = registry.register(&clip).unwrap();

    // Different length, so the stamp differs regardless of mtime granularity.
    fs::write(&clip, b"replacement with more bytes").unwrap();
    assert!(
        matches!(registry.resolve(&old), Err(PlaybackError::Missing)),
        "the id no longer matches the file"
    );

    let new = registry.register(&clip).unwrap();
    assert_ne!(old, new, "a changed file gets a new id");
    assert_eq!(registry.len(), 1, "the old version is replaced, not kept");
    assert!(matches!(
        registry.resolve(&old),
        Err(PlaybackError::InvalidRequest)
    ));
    assert!(registry.resolve(&new).is_ok());
}

#[test]
fn a_deleted_file_is_missing_not_silently_resolved() {
    let dir = Scratch::new("deleted");
    let clip = dir.file("clip.mp4", b"soon gone");
    let mut registry = SourceRegistry::new();
    let id = registry.register(&clip).unwrap();

    fs::remove_file(&clip).unwrap();
    assert!(matches!(registry.resolve(&id), Err(PlaybackError::Missing)));
    assert!(matches!(
        registry.register(&clip),
        Err(PlaybackError::Missing)
    ));
}

#[test]
fn empty_files_and_directories_cannot_be_registered() {
    let dir = Scratch::new("empty");
    let empty = dir.file("empty.mp4", b"");
    let mut registry = SourceRegistry::new();
    assert!(matches!(
        registry.register(&empty),
        Err(PlaybackError::Empty)
    ));
    assert!(matches!(
        registry.register(dir.path()),
        Err(PlaybackError::Empty)
    ));
    assert!(registry.is_empty());
}

#[test]
fn source_id_parse_rejects_anything_but_sha256_hex() {
    assert!(SourceId::parse(&"a".repeat(64)).is_some());
    assert!(
        SourceId::parse(&"A".repeat(64)).is_none(),
        "uppercase is not the canonical form"
    );
    assert!(SourceId::parse(&"a".repeat(63)).is_none());
    assert!(SourceId::parse(&"g".repeat(64)).is_none());
    assert!(SourceId::parse("/some/path.mp4").is_none());
}
