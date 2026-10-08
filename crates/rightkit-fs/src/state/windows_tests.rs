use super::*;
use crate::StateDir;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::fs::{symlink_file, OpenOptionsExt};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn DeviceIoControl(
        device: Handle,
        code: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_size: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}

// Junctions need no symlink privilege. Still skip cleanly on unsupported filesystems/policy.
fn junction(link: &Path, target: &Path) -> io::Result<()> {
    let canonical = target.canonicalize()?;
    let print: Vec<u16> = canonical.as_os_str().encode_wide().collect();
    let mut substitute = print.clone();
    if !substitute.starts_with(&[92, 92, 63, 92]) {
        return Err(io::Error::other(
            "junction target is not an extended local path",
        ));
    }
    substitute[1] = 63; // \\?\ -> \??\ native namespace.
    let data_length = 8 + (substitute.len() + print.len() + 2) * 2;
    if data_length > 16_376 {
        return Err(io::Error::other(
            "junction target exceeds reparse buffer cap",
        ));
    }
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&0xa000_0003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
    buffer.extend_from_slice(&(data_length as u16).to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    for value in [
        0,
        (substitute.len() * 2) as u16,
        ((substitute.len() + 1) * 2) as u16,
        (print.len() * 2) as u16,
    ] {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
    for unit in substitute
        .iter()
        .copied()
        .chain(Some(0))
        .chain(print.iter().copied())
        .chain(Some(0))
    {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
    fs::create_dir(link)?;
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .share_mode(3)
            .custom_flags(0x0200_0000 | 0x0020_0000)
            .open(link)?;
        let mut returned = 0;
        // SAFETY: synchronous handle and bounded mount-point reparse buffer.
        if unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                0x0009_00a4,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                null_mut(),
                0,
                &mut returned,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir(link);
    }
    result
}

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let guard = tempfile::tempdir().unwrap();
    let path = guard.path().canonicalize().unwrap();
    (guard, path)
}

#[test]
fn native_temp_creation_and_relative_publication() {
    let (_guard, root) = tmp();
    let state = StateDir::create(&root.join("state")).unwrap();
    let (temp, mut file) = state.create_temp("record").unwrap();
    file.write_all(b"inside").unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert_eq!(state.read(&temp, 100).unwrap(), b"inside");
    state.dir.rename_replace(&temp, "record").unwrap();
    assert!(!state.exists(&temp).unwrap());
    assert_eq!(state.read("record", 100).unwrap(), b"inside");
}

#[test]
fn ancestor_junction_swap_between_temp_writes_stays_pinned() {
    let (_guard, root) = tmp();
    let outside = root.join("outside");
    fs::create_dir_all(outside.join("state")).unwrap();
    fs::write(outside.join("state/keep"), b"outside").unwrap();
    let probe = root.join("probe");
    if let Err(e) = junction(&probe, &outside) {
        eprintln!("SKIP ancestor junction case: {e}");
        return;
    }
    fs::remove_dir(&probe).unwrap();
    let parent = root.join("parent");
    let moved = root.join("moved");
    let state = StateDir::create(&parent.join("state")).unwrap();
    state.write("keep", b"original").unwrap();
    let (temp, mut file) = state.create_temp("record").unwrap();
    file.write_all(b"before-").unwrap();
    // Deterministically attack after temp creation, before remaining bytes/publication.
    let swapped = fs::rename(&parent, &moved).is_ok();
    if swapped {
        if let Err(e) = junction(&parent, &outside) {
            drop(file);
            state.dir.unlink(&temp).unwrap();
            drop(state);
            fs::rename(&moved, &parent).unwrap();
            eprintln!("SKIP ancestor junction case: {e}");
            return;
        }
    }
    file.write_all(b"after").unwrap();
    file.sync_all().unwrap();
    drop(file);
    state.dir.rename_replace(&temp, "record").unwrap();
    state.write("later", b"pinned").unwrap();
    state.write_new("new", b"pinned").unwrap();
    let child = state.child("child").unwrap();
    child.write("nested", b"pinned").unwrap();
    assert_eq!(state.read("record", 100).unwrap(), b"before-after");
    assert!(state.remove("keep").unwrap());
    assert!(!state.remove("keep").unwrap());
    let original = if swapped {
        moved.join("state")
    } else {
        parent.join("state")
    };
    assert_eq!(fs::read(original.join("record")).unwrap(), b"before-after");
    assert_eq!(fs::read(original.join("later")).unwrap(), b"pinned");
    assert_eq!(fs::read(original.join("new")).unwrap(), b"pinned");
    assert_eq!(fs::read(original.join("child/nested")).unwrap(), b"pinned");
    assert_eq!(fs::read(outside.join("state/keep")).unwrap(), b"outside");
    assert_eq!(fs::read_dir(outside.join("state")).unwrap().count(), 1);
    let mut names = state.list(10).unwrap();
    names.sort();
    assert_eq!(names, ["child", "later", "new", "record"]);
    drop((child, state));
    if swapped {
        fs::remove_dir(&parent).unwrap();
    }
}

#[test]
fn pinned_directory_itself_cannot_be_renamed_or_removed() {
    let (_guard, root) = tmp();
    let path = root.join("state");
    let state = StateDir::create(&path).unwrap();
    assert!(fs::rename(&path, root.join("moved")).is_err());
    assert!(fs::remove_dir(&path).is_err());
    state.write("record", b"inside").unwrap();
    assert_eq!(fs::read(path.join("record")).unwrap(), b"inside");
    drop(state);
    fs::rename(&path, root.join("moved")).unwrap();
}

#[test]
fn target_symlink_swap_mid_write_is_refused() {
    let (_guard, root) = tmp();
    let outside = root.join("victim");
    fs::write(&outside, b"intact").unwrap();
    let state = StateDir::create(&root.join("state")).unwrap();
    state.write("record", b"old").unwrap();
    let (temp, mut file) = state.create_temp("record").unwrap();
    file.write_all(b"before-").unwrap();
    fs::remove_file(state.path().join("record")).unwrap();
    if let Err(e) = symlink_file(&outside, state.path().join("record")) {
        drop(file);
        state.dir.unlink(&temp).unwrap();
        eprintln!("SKIP file symlink case: {e}");
        return;
    }
    file.write_all(b"after").unwrap();
    drop(file);
    assert!(matches!(
        state.dir.rename_replace(&temp, "record"),
        Err(FsError::Symlink(_))
    ));
    state.dir.unlink(&temp).unwrap();
    assert!(matches!(
        state.write("record", b"attack"),
        Err(FsError::Symlink(_))
    ));
    assert!(state.write_new("record", b"attack").is_err());
    assert!(matches!(
        state.read("record", 100),
        Err(FsError::Symlink(_))
    ));
    assert!(matches!(state.remove("record"), Err(FsError::Symlink(_))));
    assert_eq!(fs::read(&outside).unwrap(), b"intact");
    assert!(fs::symlink_metadata(state.path().join("record"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(state.list(10).unwrap(), ["record"]);
    fs::remove_file(state.path().join("record")).unwrap();
}

#[test]
fn target_junction_swap_mid_write_is_refused() {
    let (_guard, root) = tmp();
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("victim"), b"intact").unwrap();
    let state = StateDir::create(&root.join("state")).unwrap();
    state.write("record", b"old").unwrap();
    let (temp, mut file) = state.create_temp("record").unwrap();
    file.write_all(b"before-").unwrap();
    fs::remove_file(state.path().join("record")).unwrap();
    if let Err(e) = junction(&state.path().join("record"), &outside) {
        drop(file);
        state.dir.unlink(&temp).unwrap();
        eprintln!("SKIP target junction case: {e}");
        return;
    }
    file.write_all(b"after").unwrap();
    drop(file);
    assert!(matches!(
        state.dir.rename_replace(&temp, "record"),
        Err(FsError::Symlink(_))
    ));
    state.dir.unlink(&temp).unwrap();
    assert!(state.write("record", b"attack").is_err());
    assert!(state.write_new("record", b"attack").is_err());
    assert!(state.read("record", 100).is_err());
    assert!(state.remove("record").is_err());
    assert!(matches!(state.child("record"), Err(FsError::Symlink(_))));
    assert!(matches!(
        StateDir::open(&state.path().join("record")),
        Err(FsError::Symlink(_))
    ));
    assert!(matches!(
        StateDir::create(&state.path().join("record/new")),
        Err(FsError::Symlink(_))
    ));
    assert_eq!(fs::read(outside.join("victim")).unwrap(), b"intact");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(state.list(10).unwrap(), ["record"]);
    fs::remove_dir(state.path().join("record")).unwrap();
}

#[test]
fn windows_names_and_no_replace_publication() {
    let (_guard, root) = tmp();
    let state = StateDir::create(&root.join("state")).unwrap();
    for name in [
        "record:stream",
        "record.",
        "record ",
        "x*y",
        "x?y",
        "x\u{1f}y",
    ] {
        assert!(matches!(
            state.write(name, b"refused"),
            Err(FsError::BadName(_))
        ));
    }
    let name = "x".repeat(255); // Internal temp suffix must not shrink public name cap.
    state.write_new(&name, b"first").unwrap();
    assert!(matches!(
        state.write_new(&name, b"second"),
        Err(FsError::AlreadyExists(_))
    ));
    assert_eq!(state.read(&name, 100).unwrap(), b"first");
    state.write(&name, b"replace").unwrap();
    assert_eq!(state.read(&name, 100).unwrap(), b"replace");
    assert!(matches!(state.list(0), Err(FsError::TooManyEntries(0))));
    assert_eq!(state.list(1).unwrap(), [name]);
}

#[test]
fn windows_name_limit_counts_utf16_units_and_keeps_temps_sweepable() {
    let (_guard, root) = tmp();
    let state = StateDir::create(&root.join("state")).unwrap();
    for name in [
        "\u{e9}".repeat(255),
        format!("{}x", "\u{1f600}".repeat(127)),
    ] {
        assert_eq!(name.encode_utf16().count(), 255);
        state.write_new(&name, b"first").unwrap();
        assert_eq!(state.read(&name, 100).unwrap(), b"first");
        state.write(&name, b"replace").unwrap();
        assert_eq!(state.read(&name, 100).unwrap(), b"replace");
        assert_eq!(state.list(1).unwrap(), std::slice::from_ref(&name));
        assert!(state.remove(&name).unwrap());
        let (temp, file) = state.create_temp(&name).unwrap();
        assert!(temp.encode_utf16().count() <= 255);
        drop(file);
        assert_eq!(state.sweep_temps(std::time::Duration::ZERO).unwrap(), 1);
        assert!(state.list(0).unwrap().is_empty());
    }
    for name in [
        "x".repeat(256),
        "\u{e9}".repeat(256),
        "\u{1f600}".repeat(128),
    ] {
        assert!(matches!(
            state.write(&name, b"refused"),
            Err(FsError::BadName(_))
        ));
        assert!(matches!(
            state.write_new(&name, b"refused"),
            Err(FsError::BadName(_))
        ));
        assert!(matches!(state.child(&name), Err(FsError::BadName(_))));
    }
}
