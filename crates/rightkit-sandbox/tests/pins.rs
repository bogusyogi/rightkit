//! Pinned read roots through the public API. One test, because the pin table is process-wide and
//! `release_all` must not race another test's handle.

use rightkit_sandbox::*;
use std::fs;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rk-pin-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

#[test]
fn pins_are_shared_counted_and_released_with_the_last_handle_or_all_at_once() {
    let sandbox = Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded));
    let python = scratch("python");
    fs::write(
        python.join(if cfg!(windows) {
            "python.exe"
        } else {
            "python3"
        }),
        "x",
    )
    .unwrap();
    let node = scratch("node");
    fs::write(
        node.join(if cfg!(windows) { "node.exe" } else { "node" }),
        "x",
    )
    .unwrap();

    let first = sandbox
        .pin_read_roots(std::slice::from_ref(&python))
        .expect("pin python");
    let second = sandbox
        .pin_read_roots(&[python.clone(), node.clone()])
        .expect("pin python again and node");
    assert_eq!(first.roots().len(), 1);
    assert_eq!(second.roots().len(), 2);

    // Dropping one of two handles on a root keeps it pinned; the other keeps node alone.
    drop(first);
    let third = sandbox
        .pin_read_roots(std::slice::from_ref(&python))
        .expect("pin after a drop");
    assert_eq!(third.roots(), [fs::canonicalize(&python).unwrap()]);

    // Everything goes at once; earlier handles are inert and their drop releases nothing twice.
    assert_eq!(PinHandle::release_all(), 2, "python and node were pinned");
    drop(second);
    drop(third);
    assert_eq!(PinHandle::release_all(), 0, "nothing left to release");

    // A pin taken after release_all works again.
    let again = sandbox
        .pin_read_roots(std::slice::from_ref(&node))
        .expect("pin node");
    assert_eq!(PinHandle::release_all(), 1);
    drop(again);

    // Too broad and not an interpreter are refused, never pinned.
    let root = if cfg!(windows) {
        PathBuf::from("C:\\")
    } else {
        PathBuf::from("/")
    };
    assert!(sandbox.pin_read_roots(&[root]).is_err());
    let plain = scratch("plain");
    fs::write(plain.join("notes.txt"), "x").unwrap();
    assert!(sandbox.pin_read_roots(&[plain]).is_err());
    assert_eq!(PinHandle::release_all(), 0);

    let _ = fs::remove_dir_all(&python);
    let _ = fs::remove_dir_all(&node);
}
