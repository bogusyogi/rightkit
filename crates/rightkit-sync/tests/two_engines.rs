//! Two engines over one local store (the "bridge" device: CloudKit and HeardRight Cloud
//! together). They share the store and nothing else: each has its own ledger and zone.
//! Proves convergence across both ecosystems, tombstones crossing both ways, no upload
//! ping-pong, and that a record one engine quarantined is never published by the other.

mod common;

use std::sync::Arc;

use common::*;
use rightkit_sync::*;

fn backend(zone: &MemoryBackend) -> Arc<dyn Backend> {
    Arc::new(zone.clone())
}

/// Device X on zone C, bridge engines EC (zone C) and EH (zone H) on one store, device Y on
/// zone H.
struct World {
    zc: MemoryBackend,
    zh: MemoryBackend,
    x: Device,
    ec: Device,
    eh: Device,
    y: Device,
    bridge: Local,
}

fn world() -> World {
    let (zc, zh) = (MemoryBackend::new(), MemoryBackend::new());
    let bridge = Local::default();
    // The bridge engines infer deletions: when one engine removes an item because of a
    // tombstone, the other sees it vanish from the shared store and tombstones it too.
    World {
        x: device(backend(&zc), false),
        ec: device_on(backend(&zc), &bridge, true),
        eh: device_on(backend(&zh), &bridge, true),
        y: device(backend(&zh), false),
        zc,
        zh,
        bridge,
    }
}

impl World {
    fn round(&mut self) {
        self.x.pass();
        self.y.pass();
        self.ec.pass();
        self.eh.pass();
        self.ec.pass();
        self.x.pass();
        self.y.pass();
    }

    /// Run rounds until a whole round writes nothing to either zone. A loop (an engine
    /// republishing what the other one just applied) never settles and fails here.
    fn settle(&mut self) {
        for _ in 0..8 {
            let before = (self.zc.saved_records(), self.zh.saved_records());
            self.round();
            if before == (self.zc.saved_records(), self.zh.saved_records()) {
                return;
            }
        }
        panic!("the engines kept writing: upload ping-pong");
    }

    fn snapshots(&self) -> [Vec<(String, String)>; 3] {
        [
            self.x.local.snapshot(),
            self.y.local.snapshot(),
            self.bridge.snapshot(),
        ]
    }
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
        .collect()
}

#[test]
fn two_ecosystems_converge_through_the_bridge_with_edits_and_deletes_both_ways() {
    let mut w = world();

    // Everything X creates reaches Y, through the bridge.
    w.x.local.put("n1", "v1", 100, 100);
    w.x.local.put("n2", "v1", 101, 101);
    w.x.local.put("n3", "v1", 102, 102);
    w.settle();
    let all = pairs(&[("n1", "v1"), ("n2", "v1"), ("n3", "v1")]);
    assert_eq!(w.snapshots(), [all.clone(), all.clone(), all]);

    // Interleaved edits on both sides: Y edits n1, X edits n2.
    w.y.local.put("n1", "y-edit", 100, 200);
    w.x.local.put("n2", "x-edit", 101, 300);
    w.settle();
    let edited = pairs(&[("n1", "y-edit"), ("n2", "x-edit"), ("n3", "v1")]);
    assert_eq!(w.snapshots(), [edited.clone(), edited.clone(), edited]);

    // Deletes cross both ways: X deletes n1 (Y's edit), Y deletes n2 (X's edit).
    w.x.local.delete("n1");
    w.x.engine.note_deleted("n1").unwrap();
    w.y.local.delete("n2");
    w.y.engine.note_deleted("n2").unwrap();
    w.settle();
    let left = pairs(&[("n3", "v1")]);
    assert_eq!(w.snapshots(), [left.clone(), left.clone(), left]);
    for name in ["n1", "n2"] {
        for zone in [&w.zc, &w.zh] {
            let t = zone.record(name).expect("tombstone in both zones");
            assert!(t.date_ms("deletedAt").is_some(), "{name} is a tombstone");
        }
    }

    // A delete on one side races an edit on the other: the tombstone wins everywhere and
    // the late edit is not published again.
    w.y.local.delete("n3");
    w.y.engine.note_deleted("n3").unwrap();
    w.x.local.put("n3", "late-edit", 102, 9_999);
    w.settle();
    assert_eq!(w.snapshots(), [vec![], vec![], vec![]]);
    for zone in [&w.zc, &w.zh] {
        let t = zone.record("n3").unwrap();
        assert!(t.date_ms("deletedAt").is_some());
        assert!(t.get("text").is_none(), "tombstone clears content");
    }

    // Nothing is left to do, and a further pass of every engine reports so.
    for d in [&mut w.x, &mut w.ec, &mut w.eh, &mut w.y] {
        let r = d.pass();
        assert_eq!(r.outcome, PassOutcome::Synced);
        assert_eq!((r.uploaded, r.downloaded, r.tombstones_written), (0, 0, 0));
    }
}

#[test]
fn a_record_one_engine_quarantined_is_never_published_by_the_other() {
    let mut w = world();
    w.x.local.put("n1", "good", 100, 100);
    // A record zone C holds that no mapper can decode (no text, no dates).
    let mut junk = RawRecord::new("Note", "bad");
    junk.set("id", FieldValue::Text("bad".into()));
    w.zc.put_record(junk);

    w.settle();

    assert!(w.ec.engine.ledger().quarantined_total >= 1);
    assert_eq!(w.eh.engine.ledger().quarantined_total, 0);
    assert!(w.zh.record("bad").is_none(), "the other engine never saw it");
    assert_eq!(
        w.zc.record("bad").unwrap().fields.len(),
        1,
        "and the quarantined record was not overwritten"
    );
    let good = pairs(&[("n1", "good")]);
    assert_eq!(w.snapshots(), [good.clone(), good.clone(), good]);
}

#[test]
fn two_engines_on_one_zone_and_one_store_do_not_fight_over_edits_and_deletes() {
    let zone = MemoryBackend::new();
    let store = Local::default();
    let mut e1 = device_on(backend(&zone), &store, true);
    let mut e2 = device_on(backend(&zone), &store, true);
    let mut other = device(backend(&zone), false);

    let settle = |e1: &mut Device, e2: &mut Device, other: &mut Device| {
        for _ in 0..8 {
            let before = zone.saved_records();
            e1.pass();
            e2.pass();
            other.pass();
            if before == zone.saved_records() {
                return;
            }
        }
        panic!("the engines kept writing: upload ping-pong");
    };

    store.put("n1", "v1", 100, 100);
    store.put("n2", "v1", 101, 101);
    store.put("n3", "v1", 102, 102);
    settle(&mut e1, &mut e2, &mut other);
    assert_eq!(other.local.len(), 3);

    // Interleaved: the store is edited, one engine is told of a delete (the other infers
    // it), and the other device edits a third item.
    store.put("n1", "edit", 100, 200);
    store.delete("n2");
    e1.engine.note_deleted("n2").unwrap();
    other.local.put("n3", "other-edit", 102, 300);
    settle(&mut e1, &mut e2, &mut other);

    let expected = pairs(&[("n1", "edit"), ("n3", "other-edit")]);
    assert_eq!(store.snapshot(), expected);
    assert_eq!(other.local.snapshot(), expected);
    assert!(zone.record("n2").unwrap().date_ms("deletedAt").is_some());
}
