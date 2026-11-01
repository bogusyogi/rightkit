use super::*;
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BI_RGB,
    DIB_RGB_COLORS,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};

struct RestoreClipboardOnDrop(Option<ClipboardSnapshot>);

impl Drop for RestoreClipboardOnDrop {
    fn drop(&mut self) {
        if let Some(snapshot) = self.0.take() {
            let sequence = clipboard_sequence_number();
            let _ = restore_general_clipboard_if_unchanged(snapshot, sequence);
        }
    }
}

unsafe fn set_unicode_only(text: &str) -> u32 {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    assert_ne!(OpenClipboard(null_mut::<c_void>() as HWND), 0);
    let result = (|| {
        if EmptyClipboard() == 0 {
            return None;
        }
        let handle = GlobalAlloc(GMEM_MOVEABLE, wide.len() * std::mem::size_of::<u16>());
        if handle.is_null() {
            return None;
        }
        let locked = GlobalLock(handle) as *mut u16;
        if locked.is_null() {
            GlobalFree(handle);
            return None;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), locked, wide.len());
        GlobalUnlock(handle);
        if SetClipboardData(CF_UNICODETEXT as u32, handle).is_null() {
            GlobalFree(handle);
            return None;
        }
        Some(())
    })();
    CloseClipboard();
    result.expect("set Unicode-only clipboard");
    GetClipboardSequenceNumber()
}

/// Reads CF_UNICODETEXT from a fresh thread, which makes Windows send
/// WM_RENDERFORMAT to the promised window (the same thing a Ctrl+V does).
fn read_unicode_text_from_other_thread() -> Option<String> {
    std::thread::spawn(|| unsafe {
        if OpenClipboard(null_mut::<c_void>() as HWND) == 0 {
            return None;
        }
        let handle =
            windows_sys::Win32::System::DataExchange::GetClipboardData(CF_UNICODETEXT as u32);
        let text = if handle.is_null() {
            None
        } else {
            let locked = GlobalLock(handle) as *const u16;
            let mut len = 0;
            while *locked.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(locked, len));
            GlobalUnlock(handle);
            Some(text)
        };
        CloseClipboard();
        text
    })
    .join()
    .ok()
    .flatten()
}

/// A consumer rendering the promised text must not defeat restoration:
/// after a receipt the prior clipboard has to come back.
#[test]
#[ignore = "touches the live clipboard; run explicitly"]
fn restore_after_render_receipt_puts_prior_clipboard_back() {
    let prior_text = "rightkit-prior-clipboard";
    let current = snapshot_general_clipboard().expect("snapshot").sequence();
    assert!(stage_clipboard_text(prior_text, current).is_ok());
    let prior = snapshot_general_clipboard().expect("snapshot");
    let mut promised =
        stage_promised_clipboard_text("rightkit-transcript", prior.sequence()).expect("stage");
    let staged_sequence = promised.sequence();
    promised.mark_posted();
    assert_eq!(
        read_unicode_text_from_other_thread().as_deref(),
        Some("rightkit-transcript")
    );
    let receipt = promised.wait_for_receipt(Duration::from_millis(500));
    assert!(
        receipt.observed(),
        "render callback must be counted as a receipt"
    );
    assert!(!receipt.ownership_lost);
    let rendered_sequence = unsafe { GetClipboardSequenceNumber() };
    eprintln!("staged_sequence={staged_sequence} rendered_sequence={rendered_sequence}");
    assert!(
        promised.restore_if_unchanged(prior),
        "restore_if_unchanged returned false after a receipt (staged={staged_sequence} now={rendered_sequence})"
    );
    assert_eq!(
        read_unicode_text_from_other_thread().as_deref(),
        Some(prior_text),
        "prior clipboard was not restored"
    );
}

/// Immediate Windows staging must publish the ANSI format before a native
/// consumer asks for it. Otherwise Windows can synthesize CF_TEXT after the
/// returned sequence and the legitimate prior-clipboard restore is rejected.
/// The guard restores the caller's clipboard even when an assertion fails.
#[test]
#[ignore = "touches the live clipboard; run explicitly"]
fn immediate_stage_cf_text_does_not_advance_sequence_after_close() {
    let original = snapshot_general_clipboard().expect("snapshot original clipboard");
    assert!(
        original.is_lossless(),
        "original clipboard snapshot must be lossless"
    );
    let original_text = read_clipboard_text();
    let mut restore = RestoreClipboardOnDrop(Some(original));

    let prior_sequence = unsafe { set_unicode_only("rightkit-prior-unicode-only") };
    let staged_sequence = stage_clipboard_text("rightkit-regression-transcript", prior_sequence)
        .expect("stage immediate clipboard text");
    assert_eq!(
        staged_sequence,
        clipboard_sequence_number(),
        "stage sequence must be valid after CloseClipboard"
    );

    let formats_present = std::thread::spawn(|| unsafe {
        if OpenClipboard(null_mut::<c_void>() as HWND) == 0 {
            return false;
        }
        let present = [1u32, 7, 16].iter().all(|format| {
            !windows_sys::Win32::System::DataExchange::GetClipboardData(*format).is_null()
        });
        CloseClipboard();
        present
    })
    .join()
    .expect("CF_TEXT reader thread");
    assert!(
        formats_present,
        "CF_TEXT/CF_OEMTEXT/CF_LOCALE must be present before native conversion"
    );
    assert_eq!(
        clipboard_sequence_number(),
        staged_sequence,
        "CF_TEXT read must not synthesize a newer clipboard generation"
    );

    let prior = snapshot_general_clipboard().expect("snapshot staged clipboard");
    let newer_sequence = stage_clipboard_text("rightkit-newer-writer", staged_sequence)
        .expect("stage newer clipboard writer");
    assert!(newer_sequence != staged_sequence);
    assert!(!restore_general_clipboard_if_unchanged(
        prior,
        staged_sequence
    ));
    assert_eq!(
        read_clipboard_text().as_deref(),
        Some("rightkit-newer-writer")
    );
    let snapshot = restore.0.take().expect("restore guard snapshot");
    assert!(restore_general_clipboard_if_unchanged(
        snapshot,
        newer_sequence
    ));
    assert_eq!(
        read_clipboard_text(),
        original_text,
        "original Unicode clipboard text must return"
    );
}

/// A CF_BITMAP is a GDI handle, not HGLOBAL bytes. Snapshotting it with
/// GlobalLock used to mark the clipboard lossy and block ordinary paste.
/// CopyImage owns an independent bitmap copy; restore transfers that copy to
/// the clipboard, and the snapshot drop path deletes it on every failure.
#[test]
#[ignore = "touches the live clipboard; run explicitly"]
fn bitmap_snapshot_round_trip_survives_text_stage_and_newer_writer() {
    let original = snapshot_general_clipboard().expect("original clipboard snapshot");
    assert!(
        original.is_lossless(),
        "refusing live test before altering unsupported clipboard"
    );
    let _restore = RestoreClipboardOnDrop(Some(original));

    unsafe {
        assert_ne!(OpenClipboard(null_mut::<c_void>() as HWND), 0);
        assert_ne!(EmptyClipboard(), 0);
        let pixels = [0x00112233u32, 0x00445566];
        let bitmap = CreateBitmap(2, 1, 1, 32, pixels.as_ptr() as *const c_void);
        assert!(!bitmap.is_null());
        assert!(!SetClipboardData(2, bitmap as _).is_null());
        CloseClipboard();
    }

    let snapshot = snapshot_general_clipboard().expect("bitmap clipboard snapshot");
    assert!(snapshot.is_lossless());
    assert_eq!(snapshot.dropped_formats(), 0);
    assert!(snapshot.format_count() >= 1);
    let staged =
        stage_clipboard_text("bitmap-round-trip", snapshot.sequence()).expect("stage transcript");
    assert!(restore_general_clipboard_if_unchanged(snapshot, staged));

    unsafe {
        assert_ne!(OpenClipboard(null_mut::<c_void>() as HWND), 0);
        let restored_bitmap = GetClipboardData(2);
        assert!(!restored_bitmap.is_null());
        let mut info = BITMAP::default();
        assert_eq!(
            GetObjectW(
                restored_bitmap,
                size_of::<BITMAP>() as i32,
                &mut info as *mut _ as *mut c_void
            ),
            size_of::<BITMAP>() as i32
        );
        assert_eq!((info.bmWidth, info.bmHeight), (2, 1));
        let dc = GetDC(null_mut());
        assert!(!dc.is_null());
        let mut dib = BITMAPINFO::default();
        dib.bmiHeader.biSize =
            size_of::<windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
        dib.bmiHeader.biWidth = 2;
        dib.bmiHeader.biHeight = 1;
        dib.bmiHeader.biPlanes = 1;
        dib.bmiHeader.biBitCount = 32;
        dib.bmiHeader.biCompression = BI_RGB;
        let mut restored_pixels = [0u8; 8];
        assert_eq!(
            GetDIBits(
                dc,
                restored_bitmap,
                0,
                1,
                restored_pixels.as_mut_ptr() as *mut _,
                &mut dib,
                DIB_RGB_COLORS
            ),
            1
        );
        assert_eq!(restored_pixels, [0x33, 0x22, 0x11, 0, 0x66, 0x55, 0x44, 0]);
        ReleaseDC(null_mut(), dc);
        CloseClipboard();
    }

    // A clipboard is shared across processes. A DIB-section handle can look
    // valid to its creator while being unusable in another application.
    use std::os::windows::process::CommandExt;
    let child = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "windows::promised_render_tests::bitmap_clipboard_child_readback",
            "--ignored",
            "--nocapture",
        ])
        .env("RIGHTKIT_BITMAP_CHILD_READBACK", "1")
        .creation_flags(0x08000000)
        .status()
        .expect("cross-process clipboard reader");
    assert!(
        child.success(),
        "restored bitmap must be readable by another process"
    );

    let stale = snapshot_general_clipboard().expect("bitmap clipboard snapshot for stale test");
    let stale_sequence = stale.sequence();
    let newer =
        stage_clipboard_text("newer-writer", stale_sequence).expect("newer clipboard writer");
    assert!(!restore_general_clipboard_if_unchanged(
        stale,
        stale_sequence
    ));
    assert_ne!(newer, stale_sequence);
}

/// Office copies carry CF_ENHMETAFILE (with a synthesised CF_METAFILEPICT). The
/// metafile handle is copied with CopyEnhMetaFileW and put back with
/// SetClipboardData; the synthesised forms must not make the snapshot lossy.
#[test]
#[ignore = "touches the live clipboard; run explicitly"]
fn enhanced_metafile_snapshot_is_lossless_and_round_trips() {
    use windows_sys::Win32::Graphics::Gdi::{CloseEnhMetaFile, CreateEnhMetaFileW, Rectangle};
    let original = snapshot_general_clipboard().expect("original clipboard snapshot");
    assert!(
        original.is_lossless(),
        "refusing live test before altering unsupported clipboard"
    );
    let _restore = RestoreClipboardOnDrop(Some(original));

    unsafe {
        let dc = CreateEnhMetaFileW(
            null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        );
        assert!(!dc.is_null());
        Rectangle(dc, 0, 0, 40, 20);
        let metafile = CloseEnhMetaFile(dc);
        assert!(!metafile.is_null());
        assert_ne!(OpenClipboard(null_mut::<c_void>() as HWND), 0);
        assert_ne!(EmptyClipboard(), 0);
        assert!(!SetClipboardData(CF_ENHMETAFILE, metafile as _).is_null());
        CloseClipboard();
    }

    let snapshot = snapshot_general_clipboard().expect("metafile clipboard snapshot");
    assert!(
        snapshot.is_lossless(),
        "dropped: {:?}",
        snapshot.dropped_format_diagnostics()
    );
    assert!(snapshot.format_count() >= 1);
    let staged =
        stage_clipboard_text("metafile-round-trip", snapshot.sequence()).expect("stage transcript");
    assert!(restore_general_clipboard_if_unchanged(snapshot, staged));

    unsafe {
        assert_ne!(OpenClipboard(null_mut::<c_void>() as HWND), 0);
        assert!(!GetClipboardData(CF_ENHMETAFILE).is_null());
        CloseClipboard();
    }
}

#[test]
#[ignore = "child of explicit live bitmap test"]
fn bitmap_clipboard_child_readback() {
    if std::env::var_os("RIGHTKIT_BITMAP_CHILD_READBACK").is_none() {
        return;
    }
    unsafe {
        assert_ne!(OpenClipboard(null_mut::<c_void>() as HWND), 0);
        let bitmap = GetClipboardData(2);
        assert!(!bitmap.is_null());
        let mut info = BITMAP::default();
        assert_eq!(
            GetObjectW(
                bitmap,
                size_of::<BITMAP>() as i32,
                &mut info as *mut _ as *mut c_void
            ),
            size_of::<BITMAP>() as i32
        );
        assert_eq!((info.bmWidth, info.bmHeight), (2, 1));
        let dc = GetDC(null_mut());
        assert!(!dc.is_null());
        let mut dib = BITMAPINFO::default();
        dib.bmiHeader.biSize =
            size_of::<windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
        dib.bmiHeader.biWidth = 2;
        dib.bmiHeader.biHeight = 1;
        dib.bmiHeader.biPlanes = 1;
        dib.bmiHeader.biBitCount = 32;
        dib.bmiHeader.biCompression = BI_RGB;
        let mut pixels = [0u8; 8];
        assert_eq!(
            GetDIBits(
                dc,
                bitmap,
                0,
                1,
                pixels.as_mut_ptr() as *mut _,
                &mut dib,
                DIB_RGB_COLORS
            ),
            1
        );
        assert_eq!(pixels, [0x33, 0x22, 0x11, 0, 0x66, 0x55, 0x44, 0]);
        ReleaseDC(null_mut(), dc);
        CloseClipboard();
    }
}

mod paste_chord_modifier_tests {
    use super::*;
    use std::cell::Cell;

    const ALL: HeldModifiers = HeldModifiers {
        ctrl: true,
        shift: true,
        alt: false,
        win: false,
    };

    fn run(states: Vec<HeldModifiers>, cap_ms: u32) -> (u32, HeldModifiers, usize) {
        let polls = Cell::new(0usize);
        let clock = Cell::new(0u32);
        let (waited, last) = settle_physical_modifiers(
            &mut || {
                let i = polls.get();
                polls.set(i + 1);
                states[i.min(states.len() - 1)]
            },
            &mut || clock.get(),
            &mut |ms| clock.set(clock.get() + ms),
            cap_ms,
            5,
        );
        (waited, last, polls.get())
    }

    #[test]
    fn settle_returns_immediately_when_nothing_held() {
        let (waited, last, polls) = run(vec![HeldModifiers::default()], 400);
        assert_eq!((waited, last.any(), polls), (0, false, 1));
    }

    #[test]
    fn settle_waits_until_physical_release() {
        let (waited, last, _) = run(vec![ALL, ALL, ALL, HeldModifiers::default()], 400);
        assert_eq!(waited, 15);
        assert!(!last.any());
    }

    #[test]
    fn settle_is_capped_and_reports_still_held() {
        let (waited, last, _) = run(vec![ALL], 400);
        assert_eq!(waited, 400);
        assert_eq!(last, ALL);
    }

    #[test]
    fn chord_with_nothing_held_is_plain_ctrl_v() {
        let inputs = build_paste_chord(HeldModifiers::default(), 0x2F, 0x1D, 0x56);
        assert_eq!(
            inputs,
            vec![
                RawInput::scan(0x1D, false, false),
                RawInput::scan(0x2F, false, false),
                RawInput::scan(0x2F, true, false),
                RawInput::scan(0x1D, true, false),
            ]
        );
    }

    #[test]
    fn chord_releases_only_held_and_represses_them_after() {
        let held = HeldModifiers {
            ctrl: true,
            shift: true,
            alt: false,
            win: false,
        };
        let inputs = build_paste_chord(held, 0x2F, 0x1D, 0x56);
        assert_eq!(
            inputs,
            vec![
                RawInput::scan(0x1D, false, false),
                RawInput::key_up(VK_SHIFT),
                RawInput::scan(0x2F, false, false),
                RawInput::scan(0x2F, true, false),
                RawInput::scan(0x1D, true, false),
                RawInput::key_down(VK_CONTROL),
                RawInput::key_down(VK_SHIFT),
            ]
        );
        assert!(!inputs.contains(&RawInput::key_up(VK_MENU)));
    }

    #[test]
    fn chord_masks_alt_release_and_falls_back_to_vk_without_scan() {
        let held = HeldModifiers {
            ctrl: false,
            shift: false,
            alt: true,
            win: false,
        };
        let inputs = build_paste_chord(held, 0, 0, 0x56);
        assert_eq!(inputs[0], RawInput::key_down(VK_CONTROL));
        assert_eq!(inputs[1], RawInput::key_down(VK_DUMMY_MASK));
        assert_eq!(inputs[3], RawInput::key_up(VK_MENU));
        assert_eq!(*inputs.last().unwrap(), RawInput::key_down(VK_MENU));
    }

    #[test]
    fn enter_release_is_empty_when_nothing_held() {
        assert!(build_enter_release(HeldModifiers::default()).is_empty());
        assert!(repress_inputs(HeldModifiers::default()).is_empty());
    }
}

mod synthesized_formats {
    use super::*;

    fn drop_of(format: u32) -> DroppedClipboardFormat {
        DroppedClipboardFormat {
            format,
            reason: ClipboardDropReason::UnsupportedFormat,
        }
    }

    fn lost(formats: &[u32], captured: &[u32]) -> Vec<u32> {
        let dropped = formats.iter().copied().map(drop_of).collect();
        unsynthesized_drops(dropped, captured)
            .into_iter()
            .map(|drop| drop.format)
            .collect()
    }

    #[test]
    fn metafile_picture_is_synthesized_from_a_captured_enhanced_metafile() {
        assert!(lost(&[CF_METAFILEPICT], &[CF_ENHMETAFILE, 13]).is_empty());
    }

    #[test]
    fn metafile_picture_alone_is_lossy() {
        assert_eq!(lost(&[CF_METAFILEPICT], &[13]), vec![CF_METAFILEPICT]);
    }

    #[test]
    fn dib_synthesizes_bitmap_palette_and_dibv5() {
        assert!(lost(&[CF_BITMAP, CF_PALETTE, CF_DIBV5], &[CF_DIB]).is_empty());
    }

    #[test]
    fn palette_without_a_captured_dib_is_lossy() {
        assert_eq!(lost(&[CF_PALETTE], &[CF_BITMAP]), vec![CF_PALETTE]);
    }

    #[test]
    fn private_format_is_always_lossy() {
        assert_eq!(lost(&[0x0200], &[CF_ENHMETAFILE, CF_DIB]), vec![0x0200]);
    }

    #[test]
    fn enhanced_metafile_and_bitmap_have_their_own_capture_path() {
        assert!(!format_is_copyable(CF_ENHMETAFILE));
        assert!(!format_is_copyable(CF_BITMAP));
        assert!(format_is_copyable(CF_DIB));
    }
}
