//! The host can share its source registry with [`PlaybackState`]; registration, the id lookup and
//! revocation then all reach the same registry. Requires the `tauri` feature.
#![cfg(feature = "tauri")]

use rightkit_playback::tauri_commands::{PlaybackHost, PlaybackState};
use rightkit_playback::{PlaybackError, PlaybackSource, SourceRegistry};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

struct Host {
    shared: Option<Arc<Mutex<SourceRegistry>>>,
}

impl PlaybackHost for Host {
    fn source(&self, _id: &str) -> Result<PlaybackSource, String> {
        Err("not used by these tests".into())
    }
    fn cache_dir(&self) -> Result<PathBuf, String> {
        Err("not used by these tests".into())
    }
    fn ffmpeg(&self) -> Result<PathBuf, String> {
        Err("not used by these tests".into())
    }
    fn allow_asset(&self, _path: &Path) -> Result<(), String> {
        Err("not used by these tests".into())
    }
    fn registry(&self) -> Option<Arc<Mutex<SourceRegistry>>> {
        self.shared.clone()
    }
}

fn scratch_file(tag: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "rightkit-playback-shared-{tag}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("clip.mp4");
    fs::write(&file, b"not really video, but non-empty").unwrap();
    file
}

fn shared_registry() -> Arc<Mutex<SourceRegistry>> {
    Arc::new(Mutex::new(SourceRegistry::new()))
}

#[test]
fn a_host_shared_registry_sees_registrations_made_through_the_state() {
    let shared = shared_registry();
    let state = PlaybackState::new(Arc::new(Host {
        shared: Some(shared.clone()),
    }));
    let file = scratch_file("host-shared");

    let id = state.register_source(&file).unwrap();
    assert_eq!(
        shared.lock().unwrap().id_for_path(&file),
        Some(id),
        "the host's registry must hold the registration"
    );
}

#[test]
fn media_source_id_returns_only_already_registered_paths() {
    let shared = shared_registry();
    let state = PlaybackState::new(Arc::new(Host {
        shared: Some(shared),
    }));
    let file = scratch_file("lookup");
    let other = scratch_file("lookup-other");

    assert!(matches!(
        state.media_source_id(&file),
        Err(PlaybackError::InvalidRequest)
    ));

    let id = state.register_source(&file).unwrap();
    assert_eq!(state.media_source_id(&file).unwrap(), id);
    assert!(
        matches!(
            state.media_source_id(&other),
            Err(PlaybackError::InvalidRequest)
        ),
        "an unregistered file must not be looked up into a source"
    );
}

#[test]
fn revocation_reaches_the_shared_registry() {
    let shared = shared_registry();
    let state = PlaybackState::new(Arc::new(Host {
        shared: Some(shared.clone()),
    }));
    let file = scratch_file("revoke");
    let id = state.register_source(&file).unwrap();

    assert!(state.revoke_source(id.as_str()));
    assert!(matches!(
        shared.lock().unwrap().resolve(&id),
        Err(PlaybackError::InvalidRequest)
    ));
    assert!(matches!(
        state.media_source_id(&file),
        Err(PlaybackError::InvalidRequest)
    ));
    assert!(file.exists(), "revoking a source never deletes the file");
}

#[test]
fn with_registry_takes_precedence_over_the_host_and_shares_the_given_registry() {
    let given = shared_registry();
    let host_registry = shared_registry();
    let state = PlaybackState::with_registry(
        Arc::new(Host {
            shared: Some(host_registry.clone()),
        }),
        given.clone(),
    );
    let file = scratch_file("with-registry");

    let id = state.register_source(&file).unwrap();
    assert_eq!(given.lock().unwrap().id_for_path(&file), Some(id));
    assert!(host_registry.lock().unwrap().is_empty());
}

#[test]
fn a_host_without_a_registry_still_gets_a_private_one() {
    let state = PlaybackState::new(Arc::new(Host { shared: None }));
    let file = scratch_file("private");

    let id = state.register_source(&file).unwrap();
    assert_eq!(state.media_source_id(&file).unwrap(), id);
    assert!(state.revoke_source(id.as_str()));
}

#[test]
fn authoriser_set_on_state_reaches_the_shared_registry() {
    let file = scratch_file("authorised");
    let registry = shared_registry();
    let state = PlaybackState::with_registry(Arc::new(Host { shared: None }), registry.clone());

    assert_eq!(
        state.authorised_media_source_id(&file),
        Err(PlaybackError::NotAllowed),
        "no hook, no registration"
    );
    assert_eq!(
        state.media_source_id(&file),
        Err(PlaybackError::InvalidRequest),
        "the original command still never registers"
    );

    state.allow_roots([file.parent().unwrap().to_path_buf()]);
    let id = state.authorised_media_source_id(&file).unwrap();
    assert_eq!(state.media_source_id(&file), Ok(id.clone()));
    assert!(registry.lock().unwrap().resolve(&id).is_ok());

    state.clear_authoriser();
    assert_eq!(
        registry.lock().unwrap().resolve(&id),
        Err(PlaybackError::NotAllowed)
    );
}
