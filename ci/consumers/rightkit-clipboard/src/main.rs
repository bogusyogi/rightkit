// rightkit-clipboard 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// `read`, `read_with`, `write`, `formats` and `platform_snapshot` touch the real system clipboard, so
// this consumer never calls them and never changes or reads what the person copied. It exercises the
// pure helpers, the content types and the read-only stale-file purge against a directory that does not
// exist.
use std::time::Duration;

use rightkit_clipboard::{
    clipboard_temp_dir, dib_to_bmp, named_board_refusal, purge_stale, Board, ClipboardContent,
    ClipboardImage, ClipboardOptions, ClipboardSnapshot, ImagePreference, DEFAULT_PURGE_AFTER,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

// A 1x1, 24-bit, uncompressed DIB: a 40-byte BITMAPINFOHEADER and one padded 4-byte pixel row.
fn dib_1x1() -> Vec<u8> {
    let mut dib = vec![0u8; 40];
    dib[0..4].copy_from_slice(&40u32.to_le_bytes());
    dib[4..8].copy_from_slice(&1i32.to_le_bytes());
    dib[8..12].copy_from_slice(&1i32.to_le_bytes());
    dib[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib[14..16].copy_from_slice(&24u16.to_le_bytes());
    dib[20..24].copy_from_slice(&4u32.to_le_bytes());
    dib.extend_from_slice(&[0x00, 0x00, 0xFF, 0x00]);
    dib
}

fn main() {
    // Private board names: system pasteboards and blanks are refused before any pasteboard is touched.
    check("blank name refused", named_board_refusal("  ").is_some());
    check(
        "system pasteboard name refused",
        named_board_refusal("Apple CFPasteboard general").is_some()
            && named_board_refusal("  apple cfpasteboard find").is_some(),
    );
    check(
        "private name accepted",
        named_board_refusal("com.example.staging").is_none(),
    );

    // DIB to BMP: header prepended, offsets computed, bad blobs refused.
    let dib = dib_1x1();
    let bmp = dib_to_bmp(&dib).expect("a valid DIB converts");
    check(
        "bmp header",
        &bmp[0..2] == b"BM"
            && u32::from_le_bytes([bmp[2], bmp[3], bmp[4], bmp[5]]) as usize == 14 + dib.len()
            && u32::from_le_bytes([bmp[10], bmp[11], bmp[12], bmp[13]]) == 14 + 40
            && bmp[14..] == dib[..],
    );
    check("short blob refused", dib_to_bmp(&[0u8; 39]).is_none());
    let mut small_header = dib.clone();
    small_header[0..4].copy_from_slice(&12u32.to_le_bytes());
    check(
        "header smaller than 40 refused",
        dib_to_bmp(&small_header).is_none(),
    );
    let mut palette_without_data = dib.clone();
    palette_without_data[14..16].copy_from_slice(&8u16.to_le_bytes());
    check(
        "palette larger than the blob refused",
        dib_to_bmp(&palette_without_data).is_none(),
    );

    // Types and defaults.
    check(
        "default board content is empty",
        ClipboardContent::default()
            == ClipboardContent {
                text: None,
                html: None,
                png: None,
                files: Vec::new(),
                custom: Default::default(),
            },
    );
    check(
        "options default to PNG first and no custom capture",
        ClipboardOptions::default().image_preference == ImagePreference::PngFirst
            && ClipboardOptions::default().custom_max_bytes.is_none(),
    );
    check(
        "boards compare",
        Board::General != Board::Named("x".to_string()),
    );
    let snapshot = ClipboardSnapshot {
        image: Some(ClipboardImage {
            extension: "png".to_string(),
            bytes: vec![1, 2, 3],
        }),
        ..ClipboardSnapshot::default()
    };
    check(
        "snapshot defaults",
        snapshot.files.is_empty() && snapshot.text.is_none() && snapshot.image.is_some(),
    );
    check(
        "purge age",
        DEFAULT_PURGE_AFTER == Duration::from_secs(3600),
    );

    // Temp directory naming and a purge of a directory that does not exist (nothing is read or removed).
    let dir = clipboard_temp_dir("rightkit-clipboard-consumer");
    check(
        "temp directory name",
        dir == std::env::temp_dir().join("rightkit-clipboard-consumer-clipboard"),
    );
    let missing = std::env::temp_dir().join(format!(
        "rightkit-clipboard-consumer-{}",
        std::process::id()
    ));
    check(
        "purging a missing directory removes nothing",
        purge_stale(&missing, Duration::ZERO) == 0,
    );
    check("the missing directory was not created", !missing.exists());

    println!("OK rightkit-clipboard 0.1.0 (registry consumer, public API)");
}
