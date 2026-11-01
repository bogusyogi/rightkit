use crate::{report_ytdlp_line, report_ytdlp_progress, stderr_tail};

#[test]
fn stderr_tail_keeps_short_text_whole() {
    assert_eq!(stderr_tail("ERROR: boom"), "ERROR: boom");
}

#[test]
fn stderr_tail_caps_long_text_to_the_newest_2_kib() {
    let noisy = format!("{}ERROR: final line", "p".repeat(10_000));
    let tail = stderr_tail(&noisy);
    assert_eq!(tail.len(), 2048);
    assert!(tail.ends_with("ERROR: final line"));
}

#[test]
fn stderr_tail_respects_utf8_boundaries() {
    // Multi-byte characters must not be split by the byte cut.
    let noisy = "é".repeat(3_000);
    let tail = stderr_tail(&noisy);
    assert!(tail.len() <= 2048);
    assert!(tail.chars().all(|c| c == 'é'));
}

#[test]
fn progress_buffer_stays_bounded_for_an_unterminated_line() {
    let mut buffer = String::new();
    let mut calls = 0usize;
    let mut progress = |_: &str, _: u8, _: &str| calls += 1;
    let chunk = vec![b'x'; 8_192];
    for _ in 0..16 {
        report_ytdlp_progress(&chunk, &mut buffer, &mut progress);
    }
    assert!(
        buffer.len() <= 4096,
        "pending line grew to {}",
        buffer.len()
    );
    assert_eq!(calls, 0);
}

#[test]
fn stderr_download_line_maps_to_the_download_stage() {
    let mut reported = Vec::new();
    let mut progress =
        |stage: &str, pct: u8, msg: &str| reported.push((stage.to_string(), pct, msg.to_string()));
    report_ytdlp_line("[download]  50.0% of 10.00MiB at 1.00MiB/s", &mut progress);
    // 10 + 50 * 48 / 100 = 34
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].0, "download");
    assert_eq!(reported[0].1, 34);
    assert!(reported[0].2.starts_with("download 50.0%"));
}

#[test]
fn stderr_non_progress_line_reports_nothing() {
    let mut calls = 0usize;
    let mut progress = |_: &str, _: u8, _: &str| calls += 1;
    report_ytdlp_line("ERROR: unable to download video", &mut progress);
    assert_eq!(calls, 0);
}
