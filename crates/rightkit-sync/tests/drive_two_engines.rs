//! Two (and three) devices syncing one record zone through `DriveBackend` over the fake Drive
//! (tests/common/fake_drive.rs): edits, deletes, tombstones that cross devices, a late device
//! that replays the zone, and the engine's conflict contract against the backend's conflicts.

mod common;

use std::sync::Arc;

use common::fake_drive::FakeDrive;
use common::*;
use rightkit_http::testing::loopback_config;
use rightkit_http::Client;
use rightkit_sync::drive::*;
use rightkit_sync::wire::WireRecord;
use rightkit_sync::*;

fn drive_backend(d: &FakeDrive) -> Arc<dyn Backend> {
    let mut cfg = DriveConfig::new("notes");
    cfg.api_base = d.base_url();
    // A small page so the listing and the change feed span several pages.
    cfg.page_size = 2;
    Arc::new(DriveBackend::new(
        Client::new(loopback_config()),
        Arc::new(FnToken(|| Ok(Some("t1".to_string())))),
        cfg,
    ))
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
        .collect()
}

/// Passes of every device until a whole sweep writes nothing to Drive. A loop (devices
/// republishing what the other just applied) never settles and fails here.
fn settle(d: &FakeDrive, devices: &mut [&mut Device]) {
    for _ in 0..10 {
        let before = d.writes();
        for _ in 0..2 {
            for dev in devices.iter_mut() {
                dev.pass();
            }
        }
        if before == d.writes() {
            return;
        }
    }
    panic!("the devices kept writing: upload ping-pong");
}

fn note_wire(name: &str, updated: i64, text: &str) -> Vec<u8> {
    let mut r = RawRecord::new("Note", name);
    r.set("id", FieldValue::Text(name.into()));
    r.set("text", FieldValue::Text(text.into()));
    r.set("createdAt", FieldValue::Date(100));
    r.set("updatedAt", FieldValue::Date(updated));
    serde_json::to_vec(&WireRecord::from(&r)).unwrap()
}

#[test]
fn two_devices_converge_through_drive_with_edits_deletes_and_a_late_joiner() {
    let d = FakeDrive::start(&["t1"]);
    let mut x = device(drive_backend(&d), false);
    let mut y = device(drive_backend(&d), false);

    x.local.put("n1", "v1", 100, 100);
    x.local.put("n2", "v1", 101, 101);
    x.local.put("n3", "v1", 102, 102);
    settle(&d, &mut [&mut x, &mut y]);
    let all = pairs(&[("n1", "v1"), ("n2", "v1"), ("n3", "v1")]);
    assert_eq!(x.local.snapshot(), all);
    assert_eq!(y.local.snapshot(), all);
    assert_eq!(d.record_files(), 3);

    // Interleaved edits: Y edits n1, X edits n2.
    y.local.put("n1", "y-edit", 100, 200);
    x.local.put("n2", "x-edit", 101, 300);
    settle(&d, &mut [&mut x, &mut y]);
    let edited = pairs(&[("n1", "y-edit"), ("n2", "x-edit"), ("n3", "v1")]);
    assert_eq!(x.local.snapshot(), edited);
    assert_eq!(y.local.snapshot(), edited);

    // Deletes cross both ways and the tombstones stay in Drive.
    x.local.delete("n1");
    x.engine.note_deleted("n1").unwrap();
    y.local.delete("n2");
    y.engine.note_deleted("n2").unwrap();
    settle(&d, &mut [&mut x, &mut y]);
    let left = pairs(&[("n3", "v1")]);
    assert_eq!(x.local.snapshot(), left);
    assert_eq!(y.local.snapshot(), left);
    assert_eq!(
        d.record_files(),
        3,
        "tombstones are records, not Drive deletes"
    );
    for name in ["n1.json", "n2.json"] {
        let v = d.content_json(name).unwrap();
        assert!(
            v["fields"]["deletedAt"].is_object(),
            "{name} is a tombstone: {v}"
        );
        assert!(
            v["fields"].get("text").is_none(),
            "{name}: tombstone clears content"
        );
    }

    // A delete races a later edit: the tombstone wins everywhere.
    y.local.delete("n3");
    y.engine.note_deleted("n3").unwrap();
    x.local.put("n3", "late-edit", 102, 9_999);
    settle(&d, &mut [&mut x, &mut y]);
    assert!(x.local.snapshot().is_empty());
    assert!(y.local.snapshot().is_empty());

    // New content, then a device that joins late replays the zone (tombstones and all).
    x.local.put("n4", "fresh", 500, 500);
    settle(&d, &mut [&mut x, &mut y]);
    let mut z = device(drive_backend(&d), false);
    settle(&d, &mut [&mut x, &mut y, &mut z]);
    let expected = pairs(&[("n4", "fresh")]);
    assert_eq!(z.local.snapshot(), expected);
    assert_eq!(x.local.snapshot(), expected);
    assert_eq!(y.local.snapshot(), expected);

    // Nothing is left to do.
    for dev in [&mut x, &mut y, &mut z] {
        let r = dev.pass();
        assert_eq!(r.outcome, PassOutcome::Synced);
        assert_eq!((r.uploaded, r.downloaded, r.tombstones_written), (0, 0, 0));
    }
}

#[test]
fn a_conflict_found_by_the_version_check_is_resolved_by_the_conflict_policy() {
    for remote_updated in [700, 50] {
        let d = FakeDrive::start(&["t1"]);
        let mut x = device(drive_backend(&d), false);
        let mut y = device(drive_backend(&d), false);
        x.local.put("n1", "v1", 100, 100);
        settle(&d, &mut [&mut x, &mut y]);
        let id = d.ids_named("n1.json").remove(0);

        // X edits locally; before X's save is checked another device writes the record.
        x.local.put("n1", "x-local", 100, 500);
        let remote = note_wire("n1", remote_updated, "other-device");
        d.hook_before_metadata({
            let id = id.clone();
            move |s| s.patch(&id, remote)
        });
        let report = x.pass();
        assert!(
            !matches!(report.outcome, PassOutcome::Failed(_)),
            "the conflict is not a failure: {:?}",
            report.outcome
        );

        if remote_updated > 500 {
            // The other device is newer: last-writer-wins takes its record, nothing is uploaded.
            assert_eq!(x.local.text("n1").as_deref(), Some("other-device"));
            assert_eq!(
                d.content_json("n1.json").unwrap()["fields"]["text"]["v"],
                "other-device"
            );
        } else {
            // X is newer: the engine re-saves on top of the server's tag.
            assert_eq!(x.local.text("n1").as_deref(), Some("x-local"));
            assert_eq!(
                d.content_json("n1.json").unwrap()["fields"]["text"]["v"],
                "x-local"
            );
        }
        settle(&d, &mut [&mut x, &mut y]);
        let want = if remote_updated > 500 {
            "other-device"
        } else {
            "x-local"
        };
        assert_eq!(x.local.text("n1").as_deref(), Some(want));
        assert_eq!(y.local.text("n1").as_deref(), Some(want));
    }
}

#[test]
fn the_zone_is_rebuilt_from_local_data_after_the_app_data_is_cleared() {
    let d = FakeDrive::start(&["t1"]);
    let mut x = device(drive_backend(&d), false);
    x.local.put("n1", "v1", 100, 100);
    x.local.put("n2", "v1", 101, 101);
    settle(&d, &mut [&mut x]);
    assert_eq!(d.record_files(), 2);

    // The user clears the app's hidden Drive data.
    d.delete_app_data();
    assert_eq!(d.record_files(), 0);
    settle(&d, &mut [&mut x]);
    assert_eq!(d.record_files(), 2, "the local items were republished");
    assert_eq!(x.local.snapshot(), pairs(&[("n1", "v1"), ("n2", "v1")]));
}
