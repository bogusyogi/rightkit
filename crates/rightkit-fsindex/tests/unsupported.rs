mod common;
use common::*;
use rightkit_fsindex::backend::windows_stub::UnsupportedBackend;
use rightkit_fsindex::{Error, Index};
use std::sync::Arc;

#[test]
fn unsupported_backend_returns_unsupported() {
    let t = tree();
    match Index::open_with_backend(static_config(&t.root), Arc::new(UnsupportedBackend)) {
        Err(Error::Unsupported(_)) => {}
        other => panic!("expected Unsupported, got {:?}", other.map(|_| ())),
    }
}

/// Off macOS and Windows the default backend is the stub, so opening an index reports Unsupported.
#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn default_backend_is_unsupported_off_macos() {
    let t = tree();
    match Index::open(static_config(&t.root)) {
        Err(Error::Unsupported(_)) => {}
        other => panic!("expected Unsupported, got {:?}", other.map(|_| ())),
    }
}
