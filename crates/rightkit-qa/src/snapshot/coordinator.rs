//! Durable schema-2 coordination. No secret parser or protocol fallback is used here.
use super::*;
use owner::schema2::{self, Operation};

pub(super) fn validate(marker: &Marker) -> Result<()> {
    let p = &marker.owner_progress;
    if marker.credential_protocol == CredentialProtocol::Schema1 {
        if p.snapshot_requested
            || p.credentials_restored
            || !p.roots_verified.is_empty()
            || p.android_verified
            || p.completion_requested
            || p.completed
        {
            return err("schema-1 marker contains schema-2 progress; marker retained");
        }
        return Ok(());
    }
    if !single_component(&marker.app)
        || !single_component(&marker.snapshot_id)
        || !Path::new(&marker.snapshot_dir).is_absolute()
        || Path::new(&marker.snapshot_dir)
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || marker.credential_owner.is_none()
        || marker.credentials.is_empty()
        || (marker.credentials_sha256.is_some() && !p.snapshot_requested)
        || ((p.credentials_restored || !p.roots_verified.is_empty() || p.android_verified)
            && (marker.credentials_sha256.is_none() || marker.state != STATE_RESTORING))
        || p.roots_verified
            .iter()
            .any(|i| !marker.roots.iter().any(|r| r.index == *i))
        || (p.android_verified && marker.android.is_none())
        || (p.completion_requested
            && (!restores_verified(marker) || marker.state != STATE_RESTORING))
        || (p.completed && !p.completion_requested)
    {
        return err("schema-2 marker has inconsistent custody/restore progress; marker retained");
    }
    Ok(())
}

fn restores_verified(marker: &Marker) -> bool {
    marker.owner_progress.credentials_restored
        && marker
            .roots
            .iter()
            .all(|r| marker.owner_progress.roots_verified.contains(&r.index))
        && (marker.android.is_none() || marker.owner_progress.android_verified)
}

pub(super) fn snapshot(marker_path: &Path, marker: &mut Marker) -> Result<String> {
    validate(marker)?;
    schema2::verify_owner(marker)?;
    if marker.owner_progress.snapshot_requested || marker.credentials_sha256.is_some() {
        return err("schema-2 snapshot custody was already requested; refusing to overwrite it");
    }
    // Persist intent before IPC: timeout/refusal/exit loss does not prove no custody exists.
    marker.owner_progress.snapshot_requested = true;
    write_marker(marker_path, marker)?;
    let metadata = schema2::operate(marker, Operation::Snapshot)?;
    marker.credentials_sha256 = Some(metadata.credentials_sha256.clone());
    write_marker(marker_path, marker)?;
    schema2::load_manifest(marker, &metadata.credentials_sha256)?;
    Ok(metadata.credentials_sha256)
}

/// Only the unverified stages are replayed. Completion can never cause restore replay.
pub(super) fn restore(
    marker_path: &Path,
    marker: &mut Marker,
    manifest: &Manifest,
    mut report: RestoreReport,
) -> Result<RestoreReport> {
    validate(marker)?;
    schema2::verify_owner(marker)?;
    ensure_stopped_before_restore(marker)?;
    report.completion_resumed = marker.owner_progress.credentials_restored
        || !marker.owner_progress.roots_verified.is_empty()
        || marker.owner_progress.android_verified
        || marker.owner_progress.completion_requested;
    marker.state = STATE_RESTORING.into();
    write_marker(marker_path, marker)?;
    if marker.owner_progress.completion_requested && !live_restores_match(marker, manifest)? {
        return err(
            "live data differs after ambiguous completion; custody unknown, marker retained",
        );
    }
    if !marker.owner_progress.credentials_restored {
        let metadata = schema2::operate(marker, Operation::Restore)?;
        // An owner receipt cannot stand in for missing coordinator verification.
        if metadata.already_completed {
            return err(
                "owner already completed without persisted restore checks; marker retained",
            );
        }
        marker.owner_progress.credentials_restored = true;
        write_marker(marker_path, marker)?;
        report.credentials_restored = metadata.item_count;
    }
    // Every checkpoint is historical evidence: read live state again before reusing it.
    for root in marker.roots.clone() {
        let live = Path::new(&root.live_path);
        let snapshot = Path::new(&marker.snapshot_dir)
            .join(ROOTS_DIR)
            .join(root.index.to_string());
        let entries = &manifest.roots[&root.index.to_string()];
        ensure_stopped(marker)?;
        if !root_matches(&root, entries)? {
            if marker.owner_progress.completion_requested {
                return err("live root differs after ambiguous completion; custody unknown, marker retained");
            }
            if root.file {
                restore_file_root(live, &snapshot, root.existed, entries)?;
            } else {
                restore_root(live, &snapshot, root.existed, entries, &root.exclude)?;
            }
            if !root_matches(&root, entries)? {
                return err("live root differs after restore; marker retained");
            }
            report.roots_restored += 1;
        }
        marker.owner_progress.roots_verified.insert(root.index);
        write_marker(marker_path, marker)?;
    }
    if let Some(android) = &marker.android {
        ensure_stopped(marker)?;
        if !crate::snapshot_android::verify_live(android)? {
            if marker.owner_progress.completion_requested {
                return err("Android data differs after ambiguous completion; custody unknown, marker retained");
            }
            crate::snapshot_android::restore(android, Path::new(&marker.snapshot_dir))?;
            if !crate::snapshot_android::verify_live(android)? {
                return err("Android data differs after restore; marker retained");
            }
            report.android_restored = true;
        }
        marker.owner_progress.android_verified = true;
        write_marker(marker_path, marker)?;
    }
    if !restores_verified(marker) {
        return err("schema-2 completion lacks persisted restore verification; marker retained");
    }
    ensure_stopped(marker)?;
    if !live_restores_match(marker, manifest)? {
        return err("live data changed before credential completion; marker retained");
    }
    marker.owner_progress.completion_requested = true;
    write_marker(marker_path, marker)?;
    let metadata = match schema2::complete(marker)? {
        schema2::Completion::Accepted(metadata) => metadata,
        schema2::Completion::RestoreIncomplete => {
            // Only authenticated typed refusal proves custody remains uncompleted.
            // One marker write resets both fields, allowing explicit recovery to restore again.
            marker.owner_progress.credentials_restored = false;
            marker.owner_progress.completion_requested = false;
            write_marker(marker_path, marker)?;
            return err(
                "owner proved credential restore incomplete; retry recovery with retained marker",
            );
        }
    };
    // alreadyCompleted is a receipt replay, never fresh credential verification.
    if metadata.already_completed {
        report.completion_resumed = true;
    }
    if !metadata.cleanup.is_clean() {
        return err("schema-2 owner cleanup is incomplete or unknown; marker retained");
    }
    marker.owner_progress.completed = true;
    write_marker(marker_path, marker)?;
    cleanup_completed(marker_path, marker, report)
}

/// Read-only comparison. Read failures remain errors, never a reason to overwrite live data.
fn root_matches(root: &RootRecord, entries: &[Entry]) -> Result<bool> {
    let live = Path::new(&root.live_path);
    let md = match fs::symlink_metadata(live) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(!root.existed),
        Err(e) => return Err(Error(format!("cannot inspect live restore root: {e}"))),
    };
    if !root.existed {
        return Ok(false);
    }
    refuse_reparse(live, &md)?;
    if root.file {
        if !md.file_type().is_file() {
            return Ok(false);
        }
        Ok(diff(entries, &[file_entry(live)?], true).is_empty())
    } else {
        if !md.is_dir() || md.file_type().is_symlink() {
            return Ok(false);
        }
        Ok(diff(entries, &scan_excluding(live, &root.exclude)?, true).is_empty())
    }
}

fn live_restores_match(marker: &Marker, manifest: &Manifest) -> Result<bool> {
    let mut matches = true;
    for root in &marker.roots {
        matches &= root_matches(root, &manifest.roots[&root.index.to_string()])?;
    }
    if let Some(android) = &marker.android {
        matches &= crate::snapshot_android::verify_live(android)?;
    }
    Ok(matches)
}

/// Clean owner completion was durably recorded. Retry local cleanup only, never owner effects.
pub(super) fn cleanup_completed(
    marker_path: &Path,
    marker: &Marker,
    report: RestoreReport,
) -> Result<RestoreReport> {
    validate(marker)?;
    if !marker.owner_progress.completed {
        return err("schema-2 cleanup lacks completion receipt; marker retained");
    }
    let snapshot = Path::new(&marker.snapshot_dir);
    let app_dir = marker_path
        .parent()
        .ok_or_else(|| Error("marker lacks app directory".into()))?;
    // Cleanup recovery may have no snapshot directory left. Recheck extant ancestors directly.
    let mut ancestor = PathBuf::new();
    for component in app_dir.components() {
        ancestor.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        let md = ctx(
            fs::symlink_metadata(&ancestor),
            "cannot stat cleanup ancestor",
            &ancestor,
        )?;
        refuse_reparse(&ancestor, &md)?;
        if !md.is_dir() || md.file_type().is_symlink() {
            return err("schema-2 cleanup ancestor is not a real directory; marker retained");
        }
    }
    fs_ctx(
        rightkit_fs::private::verify_owner_only(app_dir),
        "cleanup app directory is not private",
        app_dir,
    )?;
    fs_ctx(
        rightkit_fs::private::verify_owner_only(marker_path),
        "cleanup marker is not private",
        marker_path,
    )?;
    match fs::symlink_metadata(snapshot) {
        Ok(md) => {
            refuse_reparse(snapshot, &md)?;
            if !md.is_dir() || md.file_type().is_symlink() {
                return err("schema-2 cleanup snapshot is not a real directory; marker retained");
            }
            remove_tree(snapshot)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Error(format!(
                "cannot inspect schema-2 cleanup snapshot: {e}"
            )))
        }
    }
    // Last: any local cleanup failure leaves its durable completion receipt available.
    ctx(
        fs::remove_file(marker_path),
        "cannot remove completed restore marker",
        marker_path,
    )?;
    Ok(report)
}
