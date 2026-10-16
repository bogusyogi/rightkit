//! The public reveal and open helpers refuse paths outside the caller's roots before starting
//! anything.

use rightkit_child_process::reveal::{self, PathScope, RevealAction};
use std::{fs, io::ErrorKind, path::PathBuf};

fn dir(tag: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("rightkit-reveal-it-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn reveal_and_open_refuse_a_path_outside_every_root() {
    let allowed = dir("allowed");
    let elsewhere = dir("elsewhere");
    let file = elsewhere.join("secret.txt");
    fs::write(&file, b"x").unwrap();
    let scope = PathScope::new([&allowed]).unwrap();
    assert_eq!(
        reveal::reveal(&file, &scope).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(
        reveal::open(&file, &scope).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(
        reveal::launch_command(&file, &scope, RevealAction::Reveal)
            .unwrap_err()
            .kind(),
        ErrorKind::PermissionDenied
    );
    let _ = fs::remove_dir_all(&allowed);
    let _ = fs::remove_dir_all(&elsewhere);
}

#[test]
fn a_parent_traversal_out_of_the_root_is_refused() {
    let allowed = dir("traverse");
    let elsewhere = dir("traverse-out");
    let scope = PathScope::new([&allowed]).unwrap();
    let sneaky = allowed.join("..").join(elsewhere.file_name().unwrap());
    assert_eq!(
        reveal::reveal(&sneaky, &scope).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    let _ = fs::remove_dir_all(&allowed);
    let _ = fs::remove_dir_all(&elsewhere);
}
