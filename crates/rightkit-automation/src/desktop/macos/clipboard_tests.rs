use super::*;

#[test]
fn promised_receipt_ignores_reads_before_post_marker() {
    let before_post = Instant::now();
    let posted = before_post + Duration::from_millis(1);
    let state = PromisedPasteboardState {
        posted_at: Some(posted),
        receipts: vec![before_post, posted + Duration::from_millis(1)],
        ownership_lost: false,
    };
    let valid: Vec<_> = state.valid_receipts().copied().collect();
    assert_eq!(valid.len(), 1);
    assert_eq!(valid[0], posted + Duration::from_millis(1));
}

#[test]
fn promised_receipt_wait_times_out_without_post_read() {
    let state = Arc::new((Mutex::new(PromisedPasteboardState::new()), Condvar::new()));
    let promised = PromisedPasteboard {
        state,
        change_count: 1,
        provider: None,
    };
    promised.mark_posted();
    let receipt = promised.wait_for_receipt(Duration::from_millis(1));
    assert_eq!(receipt, PasteboardReceipt::default());
}

#[test]
fn promised_receipt_wait_wakes_on_post_read() {
    let state = Arc::new((Mutex::new(PromisedPasteboardState::new()), Condvar::new()));
    let promised = PromisedPasteboard {
        state: state.clone(),
        change_count: 1,
        provider: None,
    };
    promised.mark_posted();
    std::thread::spawn(move || {
        let (lock, wake) = &*state;
        let mut state = lock.lock().unwrap();
        state.receipts.push(Instant::now());
        wake.notify_all();
    });
    assert!(promised
        .wait_for_receipt(Duration::from_millis(100))
        .observed());
}

#[test]
fn clipboard_transaction_lease_serializes_internal_owners() {
    let first = acquire_clipboard_transaction();
    let acquired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let acquired_by_second = acquired.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let second = std::thread::spawn(move || {
        started_tx.send(()).expect("lease worker must start");
        let _lease = acquire_clipboard_transaction();
        acquired_by_second.store(true, std::sync::atomic::Ordering::Release);
    });

    started_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("lease worker must reach lock");
    std::thread::yield_now();
    assert!(!acquired.load(std::sync::atomic::Ordering::Acquire));
    drop(first);
    second.join().expect("clipboard lease worker must finish");
    assert!(acquired.load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn rich_snapshot_round_trip_preserves_formats_and_newer_owner() {
    let pasteboard = NSPasteboard::pasteboardWithUniqueName();
    let rtf_type = NSString::from_str("public.rtf");
    let html_type = NSString::from_str("public.html");
    let text_type = NSString::from_str("public.utf8-plain-text");
    let item = NSPasteboardItem::new();
    let rtf = br#"{\rtf1\ansi\deff0{\fonttbl{\f0 Helvetica;}}\f0\fs24 rich}"#;
    let html = br#"<html><body><p>rich</p></body></html>"#;
    assert!(item.setData_forType(&NSData::from_vec(rtf.to_vec()), &rtf_type));
    assert!(item.setData_forType(&NSData::from_vec(html.to_vec()), &html_type));
    assert!(item.setString_forType(&NSString::from_str("rich"), &text_type));
    let object = ProtocolObject::from_retained(item);
    let object_refs: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = vec![&*object];
    let objects = NSArray::from_slice(&object_refs);
    pasteboard.clearContents();
    assert!(pasteboard.writeObjects(&objects));

    let snapshot = snapshot_pasteboard(&pasteboard).expect("named pasteboard snapshot");
    let snapshot_for_newer_guard = snapshot.clone();
    assert_eq!(snapshot.items.len(), 1);
    let expected_representations = snapshot.items[0].representations.clone();
    assert!(expected_representations
        .iter()
        .any(|(type_name, _)| type_name == "public.rtf"));
    assert!(expected_representations
        .iter()
        .any(|(type_name, _)| type_name == "public.html"));
    assert!(expected_representations
        .iter()
        .any(|(type_name, _)| type_name == "public.utf8-plain-text"));

    pasteboard.clearContents();
    assert!(pasteboard.setString_forType(&NSString::from_str("temporary"), &text_type));
    let staged_change_count = pasteboard.changeCount() as i64;
    assert!(restore_pasteboard_if_unchanged(
        &pasteboard,
        snapshot,
        staged_change_count,
    ));
    let restored = pasteboard.pasteboardItems().expect("restored rich item");
    let restored_item = restored.objectAtIndex(0);
    for (type_name, expected_bytes) in expected_representations {
        let type_name = NSString::from_str(&type_name);
        assert_eq!(
            restored_item.dataForType(&type_name).unwrap().to_vec(),
            expected_bytes,
            "restored representation bytes must match for {}",
            type_name
        );
    }

    let expected_generation = pasteboard.changeCount() as i64;
    pasteboard.clearContents();
    assert!(pasteboard.setString_forType(&NSString::from_str("newer"), &text_type));
    assert!(!restore_pasteboard_if_unchanged(
        &pasteboard,
        snapshot_for_newer_guard,
        expected_generation,
    ));
    assert_eq!(
        pasteboard.stringForType(&text_type).unwrap().to_string(),
        "newer"
    );
}
