#[cfg(target_os = "windows")]
use std::path::Path;

#[cfg(target_os = "macos")]
#[test]
fn macos_save_and_legacy_both_write_to_disk() {
    assert!(!super::macos_destination_saves("clipboard"));
    assert!(super::macos_destination_saves("disk"));
    assert!(super::macos_destination_saves("both"));
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "captures current display; run only as an explicit native smoke"]
fn macos_screencapturekit_exclusion_smoke() {
    let output = std::env::var_os("RIGHTKIT_SCREENSHOT_SMOKE_OUT")
        .map(std::path::PathBuf::from)
        .expect("set RIGHTKIT_SCREENSHOT_SMOKE_OUT");
    super::capture_macos_screen(Some(&output)).expect("ScreenCaptureKit capture");
    assert!(output
        .metadata()
        .is_ok_and(|metadata| metadata.len() > 100_000));
}

#[cfg(target_os = "windows")]
#[test]
fn png_path_detection_is_case_insensitive() {
    assert!(super::is_png_path(Path::new("Screenshot.PNG")));
    assert!(super::is_png_path(Path::new("Screenshot.png")));
    assert!(!super::is_png_path(Path::new("Screenshot.jpg")));
}

#[cfg(target_os = "windows")]
#[test]
fn next_screenshot_path_allocates_after_existing_numbered_pngs() {
    let base = std::env::temp_dir().join(format!(
        "rightkit-screenshot-path-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("create temp dir");
    std::fs::write(base.join("Screenshot (1).png"), b"one").expect("write png");
    std::fs::write(base.join("Screenshot (7).PNG"), b"seven").expect("write png");
    std::fs::write(base.join("not-a-screenshot.png"), b"ignore").expect("write png");

    assert_eq!(
        super::next_screenshot_path(&base).expect("path"),
        base.join("Screenshot (8).png")
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(target_os = "windows")]
#[test]
fn decode_32bit_bottom_up_dib_to_rgba() {
    let mut dib = Vec::new();
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&2i32.to_le_bytes());
    dib.extend_from_slice(&2i32.to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&16u32.to_le_bytes());
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    // Bottom row first: blue, white.
    dib.extend_from_slice(&[255, 0, 0, 0, 255, 255, 255, 0]);
    // Top row: red, green.
    dib.extend_from_slice(&[0, 0, 255, 0, 0, 255, 0, 0]);

    let image = super::decode_dib_rgba(&dib).expect("decode DIB");

    assert_eq!(image.width, 2);
    assert_eq!(image.height, 2);
    assert_eq!(
        image.rgba,
        vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]
    );
}
