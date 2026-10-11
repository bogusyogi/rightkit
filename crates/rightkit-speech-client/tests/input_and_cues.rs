//! Platform-independent checks: the SRT cue parser on the host's own output shape, the
//! `FileTranscript::cues` view, and the pre-flight file check.

use rightkit_speech_client::*;
use std::path::Path;

#[test]
fn srt_cues_keep_host_text_literally_and_read_millisecond_stamps() {
    // The host writes SRT literally: no escaping, markup kept (its subtitle_export tests).
    let srt = "1\n00:00:00,000 --> 00:00:02,000\nAT&T <b>\n\n\
               2\n00:01:02,004 --> 01:00:00.5\nsecond line\nthird line\n\n";
    assert_eq!(
        parse_srt_cues(srt),
        vec![
            Cue {
                start_ms: 0,
                end_ms: 2_000,
                text: "AT&T <b>".into()
            },
            Cue {
                start_ms: 62_004,
                end_ms: 3_600_500,
                text: "second line\nthird line".into()
            },
        ]
    );
}

#[test]
fn srt_cue_parser_skips_unreadable_blocks_and_accepts_crlf_and_a_bom() {
    let srt = "\u{feff}1\r\n00:00:00,000 --> 00:00:01,000\r\nhello\r\n\r\n\
               2\r\nnot a timing\r\nlost\r\n\r\n\
               3\r\n00:00:0x,000 --> 00:00:02,000\r\nbad stamp\r\n\r\n\
               4\r\n00:00:02,000 --> 00:00:03,000\r\n\r\n";
    assert_eq!(
        parse_srt_cues(srt),
        vec![Cue {
            start_ms: 0,
            end_ms: 1_000,
            text: "hello".into()
        }]
    );
    assert!(parse_srt_cues("").is_empty());
}

#[test]
fn file_transcript_cues_are_the_host_srt_cues() {
    let transcript = FileTranscript {
        text: "hello from fake".into(),
        srt: "1\n00:00:00,000 --> 00:00:00,900\nhello from fake\n\n".into(),
        vtt: "WEBVTT\n\n00:00:00.000 --> 00:00:00.900\nhello from fake\n\n".into(),
        words: vec![TimedWord {
            text: "hello".into(),
            start_ms: 0,
            end_ms: 900,
        }],
    };
    assert_eq!(
        transcript.cues(),
        vec![Cue {
            start_ms: 0,
            end_ms: 900,
            text: "hello from fake".into()
        }]
    );
    let empty = FileTranscript {
        text: String::new(),
        srt: String::new(),
        vtt: String::new(),
        words: vec![],
    };
    assert!(empty.cues().is_empty());
}

#[test]
fn preflight_reports_a_missing_absolute_path_with_the_hosts_own_code_and_message() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.wav");
    assert_eq!(
        preflight_file(&missing),
        Err(ClientError::Engine {
            code: "E_ENGINE".into(),
            message: format!("file not found: {}", missing.display()),
        })
    );
}

#[test]
fn preflight_passes_an_existing_file_and_leaves_relative_paths_to_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let present = dir.path().join("present.wav");
    std::fs::write(&present, b"RIFF").unwrap();
    assert_eq!(preflight_file(&present), Ok(()));
    // A relative path resolves in the host's working directory, which this process cannot see.
    assert_eq!(preflight_file(Path::new("does-not-exist.wav")), Ok(()));
}

#[cfg(unix)]
#[test]
fn preflight_refuses_a_path_the_json_wire_cannot_carry() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let path = Path::new(OsStr::from_bytes(b"/tmp/caf\xe9.wav"));
    assert!(matches!(
        preflight_file(path),
        Err(ClientError::Unsupported(ref m)) if m.contains("UTF-8")
    ));
}
