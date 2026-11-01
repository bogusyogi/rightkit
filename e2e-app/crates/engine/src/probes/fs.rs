//! rightkit-fs (features `fs`, `lock`): atomic and owner-only writes, a pinned StateDir, cross-handle file locks, stale
//! temp sweep, path containment, name sanitising, CRC32 and a stored ZIP, on real files under this run's scratch dir.
use rightkit_fs::{
    atomic_write, atomic_write_private, contain, crc32, ensure_private_dir, sanitize_file_name, stored_zip_bytes,
    sweep_stale_temps, FileLock, FsError, LockKind, LockOptions, StateDir,
};
use serde_json::{json, Value};
use std::time::Duration;

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-fs: {what}"))
    }
}

fn e<T: std::fmt::Display>(what: &str) -> impl FnOnce(T) -> String + '_ {
    move |err| format!("rightkit-fs: {what}: {err}")
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    // Pinning refuses symlinked ancestors (macOS temp is under /var -> /private/var), so work in the resolved path.
    let dir = rightkit_fs::canonicalize_plain(ctx.dir("fs")?).map_err(e("canonicalize"))?;

    // Atomic write replaces in full and leaves no temp behind.
    let target = dir.join("doc.txt");
    atomic_write(&target, b"first").map_err(e("atomic_write 1"))?;
    atomic_write(&target, b"second version").map_err(e("atomic_write 2"))?;
    ensure(std::fs::read(&target).map_err(e("read"))? == b"second version", "atomic_write did not replace the content")?;
    let names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(e("read_dir"))?
        .filter_map(|x| x.ok().map(|x| x.file_name().to_string_lossy().into_owned()))
        .collect();
    ensure(names == ["doc.txt"], &format!("atomic_write left extra files: {names:?}"))?;

    // Owner-only write: on unix the mode is 0600 and the directory 0700.
    let secret_dir = dir.join("private");
    ensure_private_dir(&secret_dir).map_err(e("ensure_private_dir"))?;
    let secret = secret_dir.join("secret.bin");
    atomic_write_private(&secret, b"token").map_err(e("atomic_write_private"))?;
    ensure(std::fs::read(&secret).map_err(e("read secret"))? == b"token", "private content")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file_mode = std::fs::metadata(&secret).map_err(e("stat secret"))?.permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(&secret_dir).map_err(e("stat private dir"))?.permissions().mode() & 0o777;
        ensure(file_mode & 0o077 == 0, &format!("private file mode is {file_mode:o}, not owner-only"))?;
        ensure(dir_mode & 0o077 == 0, &format!("private dir mode is {dir_mode:o}, not owner-only"))?;
    }
    rightkit_fs::restrict_owner_only(&secret).map_err(e("restrict_owner_only"))?;

    // Locks: a second handle on a held lock is refused with the Locked/LockTimeout error; release makes it free.
    let lock_path = dir.join("work.lock");
    let held = FileLock::acquire_kind(&lock_path, LockKind::Flock, &LockOptions::default().labeled("e2e-probe"))
        .map_err(e("first lock"))?;
    match FileLock::try_lock_with(&lock_path, LockKind::Flock, Duration::ZERO) {
        Err(FsError::Locked { .. }) | Err(FsError::LockTimeout { .. }) => {}
        Ok(_) => return Err("rightkit-fs: a held lock was granted twice".into()),
        Err(other) => return Err(format!("rightkit-fs: contended lock failed with the wrong error: {other}")),
    }
    drop(held);
    let again = FileLock::try_lock_with(&lock_path, LockKind::Flock, Duration::from_millis(500)).map_err(e("lock after release"))?;
    drop(again);
    let guarded = rightkit_fs::with_lock(&lock_path, &LockOptions::wait(Duration::from_secs(1)), || 41 + 1).map_err(e("with_lock"))?;
    ensure(guarded == 42, "with_lock result")?;

    // StateDir: pinned directory with named reads, writes, list and remove.
    let state = StateDir::create(&dir.join("state")).map_err(e("StateDir::create"))?;
    state.write("a.json", br#"{"a":1}"#).map_err(e("state write"))?;
    state.write_new("b.json", b"{}").map_err(e("state write_new"))?;
    ensure(matches!(state.write_new("b.json", b"x"), Err(FsError::AlreadyExists(_))), "write_new replaced an existing entry")?;
    ensure(state.read("a.json", 1024).map_err(e("state read"))? == br#"{"a":1}"#, "state read back")?;
    ensure(matches!(state.read("a.json", 3), Err(FsError::TooLarge { .. })), "state read ignored its byte cap")?;
    ensure(matches!(state.write("../escape", b"x"), Err(FsError::BadName(_))), "a path-escaping name was accepted")?;
    let mut listed = state.list(10).map_err(e("state list"))?;
    listed.sort();
    ensure(listed == ["a.json", "b.json"], &format!("state list was {listed:?}"))?;
    ensure(state.remove("a.json").map_err(e("state remove"))? && !state.exists("a.json").map_err(e("exists"))?, "state remove")?;

    // A symlink at a state dir path is refused (unix: creating one needs no privilege).
    #[cfg(unix)]
    {
        let real = dir.join("real");
        std::fs::create_dir(&real).map_err(e("mkdir real"))?;
        std::os::unix::fs::symlink(&real, dir.join("linked")).map_err(e("symlink"))?;
        ensure(StateDir::open(&dir.join("linked")).is_err(), "StateDir::open followed a symlink")?;
    }

    // Stale temp sweep never removes ordinary files.
    let swept = sweep_stale_temps(&dir, Duration::from_secs(0)).map_err(e("sweep_stale_temps"))?;
    ensure(target.is_file(), "the sweep removed a live file")?;

    // Containment and names.
    let inside = contain(&dir, "state/b.json").map_err(e("contain inside"))?;
    ensure(inside.ends_with("b.json"), "contain result")?;
    ensure(contain(&dir, "../outside").is_err() && contain(&dir, "state/../../x").is_err(), "contain allowed an escape")?;
    let clean = sanitize_file_name("../../etc/pa:ss*wd?.txt");
    ensure(clean == "pa_ss_wd_.txt", &format!("sanitize_file_name gave {clean:?}"))?;
    ensure(sanitize_file_name("CON") != "CON" && sanitize_file_name("..") != "..", "reserved or dot names survived")?;

    // CRC32 check value and a stored ZIP.
    ensure(crc32(b"123456789") == 0xCBF4_3926, "crc32 check value")?;
    let zip = stored_zip_bytes(&[("a.txt", b"alpha"), ("dir/b.txt", b"beta")]).map_err(e("stored_zip_bytes"))?;
    ensure(zip.starts_with(b"PK\x03\x04") && zip.windows(5).any(|w| w == b"alpha"), "stored zip bytes")?;

    Ok(json!({ "state_entries": listed, "swept_temps": swept, "zip_bytes": zip.len(), "lock_label_owner": std::process::id() }))
}
