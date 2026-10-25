//! Public inventory/qualification APIs through real owned child processes.
#![cfg(unix)]

mod common;

use common::TestBinary;
use rightkit_ffmpeg::encoders::{
    qualify_encoder, select_qualified_delivery, Encoder, EncoderError, EncoderInventory,
};
use rightkit_ffmpeg::{CancellationToken, RunError};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

const T: Duration = Duration::from_secs(3);

fn script(path: &Path, body: &str) -> TestBinary {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    TestBinary::new(path)
}

#[test]
fn inventory_then_failed_gpu_falls_back_to_working_system_encoder() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(
        &dir.path().join("ffmpeg with spaces"),
        r#"
case " $* " in
  *" -encoders "*)
    printf ' V....D h264_nvenc NVIDIA\n V....D h264_mf MF\n VFS... prores_ks ProRes\n A....D aac AAC\n'
    ;;
  *" -c:v h264_nvenc "*) echo 'no GPU' >&2; exit 1 ;;
  *" color=c=black:s=256x256:r=30:d=0.2 -c:v h264_mf -f null - "*) exit 0 ;;
  *) exit 2 ;;
esac"#,
    );
    let cancel = CancellationToken::new();
    let inventory = EncoderInventory::probe_cached(&binary, T, &cancel).unwrap();
    assert!(inventory.has("prores_ks"));
    assert!(!inventory.has("aac"));
    let selected = select_qualified_delivery(&inventory, "windows", true, &mut |name| {
        qualify_encoder(&binary, name, T, &cancel).is_ok()
    })
    .unwrap();
    assert_eq!(selected, Encoder::MediaFoundation);
    assert!(matches!(
        qualify_encoder(&binary, "h264_nvenc", T, &cancel),
        Err(RunError::Failed { .. })
    ));
}

#[test]
fn inventory_and_qualification_propagate_cancel_timeout_and_spawn_failures() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("spawned");
    let binary = script(
        &dir.path().join("slow-ffmpeg"),
        &format!("touch '{}'\nexec sleep 30", marker.display()),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        EncoderInventory::probe_cached(&binary, T, &cancel),
        Err(EncoderError::Probe(RunError::Cancelled))
    ));
    assert!(matches!(
        qualify_encoder(&binary, "h264_mf", T, &cancel),
        Err(RunError::Cancelled)
    ));
    assert!(!marker.exists(), "cancelled probe never spawns");
    let cancel = CancellationToken::new();
    let short = Duration::from_millis(100);
    assert!(matches!(
        EncoderInventory::probe_cached(&binary, short, &cancel),
        Err(EncoderError::Probe(RunError::TimedOut(_)))
    ));
    assert!(matches!(
        qualify_encoder(&binary, "h264_mf", short, &cancel),
        Err(RunError::TimedOut(_))
    ));
    let missing = TestBinary::new(dir.path().join("missing"));
    assert!(matches!(
        EncoderInventory::probe_cached(&missing, T, &cancel),
        Err(EncoderError::Probe(RunError::Spawn(_)))
    ));
}

#[test]
fn inventory_cache_reuses_success_and_uncached_probe_sees_replacements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ffmpeg");
    let marker = dir.path().join("listed");
    let body = |name: &str| {
        format!(
            "echo listed >> '{}'\nprintf ' V....D {name} encoder\\n'",
            marker.display()
        )
    };
    let binary = script(&path, &body("h264_mf"));
    let cancel = CancellationToken::new();
    let first = EncoderInventory::probe_cached(&binary, T, &cancel).unwrap();
    assert!(first.has("h264_mf"));
    script(&path, &body("prores_ks"));
    assert_eq!(
        EncoderInventory::probe_cached(&binary, T, &cancel).unwrap(),
        first
    );
    assert_eq!(fs::read_to_string(&marker).unwrap().lines().count(), 1);
    assert!(EncoderInventory::probe_uncached(&binary, T, &cancel)
        .unwrap()
        .has("prores_ks"));
    cancel.cancel();
    assert!(matches!(
        EncoderInventory::probe_cached(&binary, T, &cancel),
        Err(EncoderError::Probe(RunError::Cancelled))
    ));
    rightkit_ffmpeg::encoders::encoder_cache_clear();
    assert!(
        EncoderInventory::probe_cached(&binary, T, &CancellationToken::new())
            .unwrap()
            .has("prores_ks")
    );
}

#[test]
fn failed_inventory_is_not_cached() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ffmpeg");
    let binary = script(&path, "exit 1");
    let cancel = CancellationToken::new();
    assert!(matches!(
        EncoderInventory::probe_cached(&binary, T, &cancel),
        Err(EncoderError::Probe(RunError::Failed { .. }))
    ));
    script(&path, "printf ' V....D prores_ks ProRes\\n'");
    assert!(EncoderInventory::probe_cached(&binary, T, &cancel)
        .unwrap()
        .has("prores_ks"));
}
