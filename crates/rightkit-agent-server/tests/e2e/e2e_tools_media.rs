//! Media tool journeys through the REAL `coderight run` binary.
//!
//! The external-binary tools (`ocr`, `transcribe`, `web_extract`, `media_fetch`,
//! `ffmpeg_extract`) are driven against a scripted stand-in executable passed through each
//! tool's own `binary` argument: the stand-in records the exact argv the tool built and (for
//! the ffmpeg / yt-dlp shapes) really writes the output file, so the tests assert the product's
//! command construction, process launch, output shaping and failure reporting — no network,
//! no media models. `ocr_onnx` runs the real in-process OS recognizer on an image the test
//! draws with the operating system's own graphics stack.
//! `wasm_compute` has no packaged runtime (it is never advertised), so it is asserted absent.
#![allow(dead_code, unused_imports)]

use crate::support::e2e_tools as t;
use t::*;

const STUB_JS: &str = r#"const fs = require('fs');
const a = process.argv.slice(2);
console.log('STUB-ARGS ' + JSON.stringify(a));
if (a.includes('--stub-fail')) { console.error('stub-failure'); process.exit(7); }
// A hung command never reaches its output writes; successful calls still write real files.
if (a.includes('--stub-sleep')) {
  setTimeout(function () {}, 60000);
} else {
  const o = a.indexOf('-o');
  if (o >= 0) fs.writeFileSync(a[o + 1], 'FAKE-MEDIA');
  if (a.includes('-y')) fs.writeFileSync(a[a.length - 1], 'FAKE-CLIP');
}
"#;

#[cfg(windows)]
const STUB_CMD: &str = "@echo off\r\nnode \"%~dp0stub.js\" %*\r\nexit /b %ERRORLEVEL%\r\n";

#[cfg(windows)]
fn stub_files() -> Vec<(&'static str, &'static str)> {
    vec![("stub.js", STUB_JS), ("stub.cmd", STUB_CMD)]
}

#[cfg(windows)]
fn stub_binary(cell: &RootCell) -> String {
    cell.root().join("stub.cmd").to_string_lossy().to_string()
}

/// The argv the stand-in recorded: the JSON array after `STUB-ARGS `.
fn stub_args(text: &str) -> Vec<String> {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("STUB-ARGS "))
        .unwrap_or_else(|| panic!("no STUB-ARGS line in {text:?}"));
    serde_json::from_str::<Vec<String>>(line).expect("STUB-ARGS is a JSON string array")
}

#[cfg(windows)]
#[test]
fn external_binary_media_tools_build_the_right_commands_and_report_real_outcomes() {
    let cell = RootCell::new();
    let bin = {
        let cell = cell.clone();
        move || stub_binary(&cell)
    };
    let (b0, b1, b2, b3, b4, b5, b6, b7, b8) = (
        bin.clone(),
        bin.clone(),
        bin.clone(),
        bin.clone(),
        bin.clone(),
        bin.clone(),
        bin.clone(),
        bin.clone(),
        bin.clone(),
    );
    let p = sequence(
        vec![
            // 0 ocr (Tesseract-shaped)
            call_with(
                "ocr",
                move |_| json!({"image_path":"scan.png","language":"eng","psm":6,"binary": b0()}),
            ),
            // 1 transcribe
            call_with(
                "transcribe",
                move |_| json!({"media_path":"talk.wav","model":"tiny","language":"en","binary": b1()}),
            ),
            // 2 web_extract
            call_with(
                "web_extract",
                move |_| json!({"url":"https://example.invalid/article","format":"markdown","binary": b2()}),
            ),
            // 3 media_fetch metadata mode
            call_with(
                "media_fetch",
                move |_| json!({"url":"https://example.invalid/video","binary": b3()}),
            ),
            // 4 media_fetch download mode writes a real file
            call_with(
                "media_fetch",
                move |_| json!({"url":"https://example.invalid/video","output_path":"fetched.bin","format":"best","binary": b4()}),
            ),
            // 5 ffmpeg_extract writes a real file
            call_with(
                "ffmpeg_extract",
                move |_| json!({"input_path":"in.mp4","output_path":"clip.wav","start":"00:00:01","duration":"2","mode":"audio","binary": b5()}),
            ),
            // 6 failing binary: exit code and stderr reach the model
            call_with(
                "web_extract",
                move |_| json!({"url":"https://example.invalid/x","extra_args":["--stub-fail"],"binary": b6()}),
            ),
            // 7 hung binary: killed at the tool's timeout
            call_with(
                "ffmpeg_extract",
                move |_| json!({"input_path":"in.mp4","output_path":"never.wav","extra_args":["--stub-sleep"],"timeout_ms":1500,"binary": b7()}),
            ),
            // 8 argument validation
            call_with(
                "ffmpeg_extract",
                move |_| json!({"input_path":"in.mp4","output_path":"x.wav","mode":"bogus","binary": b8()}),
            ),
            // 9 missing binary
            call(
                "ocr",
                json!({"image_path":"scan.png","binary":"definitely-not-installed-e2e-binary"}),
            ),
            // 10 missing required argument
            call("transcribe", json!({})),
            // 11 invalid engine
            call(
                "transcribe",
                json!({"media_path":"talk.wav","engine":"bogus"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &stub_files());
    cell.bind(&rig);
    let out = rig.run("go");
    let r = expect_results(&out, 12);

    assert!(!r[0].error, "{}", r[0].text);
    assert_eq!(
        stub_args(&r[0].text),
        vec!["scan.png", "stdout", "-l", "eng", "--psm", "6"]
    );
    assert!(!r[1].error, "{}", r[1].text);
    assert_eq!(
        stub_args(&r[1].text),
        vec!["talk.wav", "--model", "tiny", "--language", "en"]
    );
    assert!(!r[2].error, "{}", r[2].text);
    assert_eq!(
        stub_args(&r[2].text),
        vec!["--format", "markdown", "https://example.invalid/article"]
    );
    assert!(!r[3].error, "{}", r[3].text);
    assert_eq!(
        stub_args(&r[3].text),
        vec!["--dump-json", "https://example.invalid/video"]
    );
    assert!(!r[4].error, "{}", r[4].text);
    assert_eq!(
        stub_args(&r[4].text),
        vec![
            "-f",
            "best",
            "-o",
            "fetched.bin",
            "https://example.invalid/video"
        ]
    );
    assert_eq!(
        rig.read("fetched.bin"),
        "FAKE-MEDIA",
        "download must really land on disk"
    );

    assert!(!r[5].error, "{}", r[5].text);
    assert_eq!(
        stub_args(&r[5].text),
        // rightkit-ffmpeg 0.2.9 command.rs:264-269 (`seconds_arg`) renders seconds as `S.micros`.
        vec!["-y", "-ss", "1.000000", "-i", "in.mp4", "-t", "2.000000", "-vn", "clip.wav"]
    );
    assert_eq!(
        rig.read("clip.wav"),
        "FAKE-CLIP",
        "ffmpeg output must really be written"
    );

    assert!(r[6].error);
    assert!(r[6].text.starts_with("[exit 7]\n"), "{}", r[6].text);
    assert!(
        r[6].text.contains("[stderr]\nstub-failure"),
        "{}",
        r[6].text
    );

    assert!(r[7].error);
    assert!(
        r[7].text
            .starts_with("ffmpeg_extract command timed out after 1500ms:"),
        "{}",
        r[7].text
    );
    assert!(!rig.exists("never.wav"));

    assert!(r[8].error);
    assert_eq!(
        r[8].text,
        "Invalid mode: expected 'audio' or 'copy', got 'bogus'."
    );
    assert!(r[9].error);
    assert!(
        r[9].text
            .starts_with("Failed to spawn ocr binary 'definitely-not-installed-e2e-binary':"),
        "{}",
        r[9].text
    );
    assert!(r[10].error);
    // EFF-001 schema admission precedes media dispatch (tool_dispatch_plumbing.rs).
    assert!(
        r[10]
            .text
            .starts_with("Invalid tool arguments: $.media_path is required."),
        "{}",
        r[10].text
    );
    assert!(
        r[10].text.contains("Expected input schema:"),
        "{}",
        r[10].text
    );
    assert!(r[11].error);
    assert_eq!(
        r[11].text,
        "Invalid engine: expected 'whisper' or 'parakeet'."
    );
}

/// Draw "HELLO WORLD" into a PNG with the OS graphics stack, then recognise it with the
/// in-process OS OCR (`ocr_onnx` = Windows.Media.Ocr).
#[cfg(windows)]
#[test]
fn ocr_onnx_recognises_text_drawn_into_a_real_image() {
    let cell = RootCell::new();
    let draw_cell = cell.clone();
    let p = sequence(
        vec![
            call_with("powershell", move |_| {
                let png = draw_cell
                    .root()
                    .join("ocr-input.png")
                    .to_string_lossy()
                    .replace('\\', "/");
                json!({"command": format!(
                    "Add-Type -AssemblyName System.Drawing; \
                     $b = New-Object System.Drawing.Bitmap 900,220; \
                     $g = [System.Drawing.Graphics]::FromImage($b); \
                     $g.Clear([System.Drawing.Color]::White); \
                     $f = New-Object System.Drawing.Font('Arial', 72); \
                     $g.DrawString('HELLO WORLD', $f, [System.Drawing.Brushes]::Black, 20, 40); \
                     $b.Save('{png}', [System.Drawing.Imaging.ImageFormat]::Png); \
                     $g.Dispose(); $b.Dispose(); 'drawn'"
                )})
            }),
            call("ocr_onnx", json!({"image_path":"ocr-input.png"})),
            call("ocr_onnx", json!({"image_path":"no-such-image.png"})),
            call("ocr_onnx", json!({})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    cell.bind(&rig);
    let out = rig.run("go");
    let r = expect_results(&out, 4);

    assert!(!r[0].error, "{}", r[0].text);
    assert!(rig.exists("ocr-input.png"), "the test image must exist");
    assert!(!r[1].error, "{}", r[1].text);
    let recognised = r[1].text.to_uppercase();
    assert!(
        recognised.contains("HELLO") && recognised.contains("WORLD"),
        "OS OCR must read the drawn text, got: {:?}",
        r[1].text
    );
    assert!(r[2].error);
    assert!(
        r[2].text.starts_with("ocr_onnx: image not found at "),
        "{}",
        r[2].text
    );
    assert!(r[3].error);
    assert!(
        r[3].text
            .starts_with("Invalid tool arguments: $.image_path is required"),
        "schema validation should reject missing image_path: {}",
        r[3].text
    );
}

/// The WASM runtime is not packaged on any target, so the tool must never be offered to the
/// model (an advertised tool that can only fail is a lie to the model). tts / sftp_workspace
/// are likewise unregistered.
#[test]
fn tools_without_a_real_backend_are_never_advertised_to_the_model() {
    let p = sequence(
        vec![call("get_time", json!({"operation":"current_time"}))],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let _ = expect_results(&out, 1);
    let first = p.requests();
    let advertised = advertised_tools(&first[0]);
    assert!(!advertised.is_empty(), "the model must be offered tools");
    for dead in ["wasm_compute", "tts", "sftp_workspace"] {
        assert!(
            !advertised.iter().any(|n| n == dead),
            "{dead} has no backend and must not be advertised: {advertised:?}"
        );
    }
}
