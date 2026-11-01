//! The one media, image and document extension table (audit D5, #210).
//!
//! These lists used to be written five times across Rust and TypeScript and had
//! drifted: the file picker could not select `.ogg`, `.opus` or `.avi` although
//! pasting them worked. Rust code reads the constants below; the frontend reads
//! `data/media-types.json`, which [`media_types_json`] renders and a
//! test keeps in step (`RIGHTKIT_UPDATE_GENERATED=1` rewrites it).

/// Audio file extensions (lowercase, no dot).
pub const AUDIO_EXTS: &[&str] = &[
    "aac", "flac", "m4a", "mp3", "oga", "ogg", "opus", "wav", "wma",
];

/// Video file extensions (lowercase, no dot).
pub const VIDEO_EXTS: &[&str] = &["avi", "m4v", "mkv", "mov", "mp4", "webm", "wmv"];

/// Video containers that are never audio-only. `webm` is excluded on purpose: yt-dlp
/// saves audio-only downloads as `.webm`, so a `.webm` file can still be audio.
pub const VIDEO_CONTAINER_EXTS: &[&str] = &["avi", "m4v", "mkv", "mov", "mp4", "wmv"];

/// Still-image extensions the app can OCR.
pub const IMAGE_EXTS: &[&str] = &["bmp", "gif", "jpeg", "jpg", "png", "tif", "tiff"];

/// Document extensions.
pub const DOCUMENT_EXTS: &[&str] = &["pdf"];

fn contains(list: &[&str], ext: &str) -> bool {
    let ext = ext.trim().trim_start_matches('.');
    list.iter().any(|known| known.eq_ignore_ascii_case(ext))
}

/// True for an audio extension; case-insensitive, a leading dot is ignored.
pub fn is_audio_ext(ext: &str) -> bool {
    contains(AUDIO_EXTS, ext)
}

/// True for a video extension; case-insensitive, a leading dot is ignored.
pub fn is_video_ext(ext: &str) -> bool {
    contains(VIDEO_EXTS, ext)
}

/// True for a video container that is never audio-only (see [`VIDEO_CONTAINER_EXTS`]).
pub fn is_video_container_ext(ext: &str) -> bool {
    contains(VIDEO_CONTAINER_EXTS, ext)
}

/// True for an audio or video extension.
pub fn is_media_ext(ext: &str) -> bool {
    is_audio_ext(ext) || is_video_ext(ext)
}

/// True for a still-image extension; case-insensitive, a leading dot is ignored.
pub fn is_image_ext(ext: &str) -> bool {
    contains(IMAGE_EXTS, ext)
}

/// True for a document extension (`pdf`).
pub fn is_document_ext(ext: &str) -> bool {
    contains(DOCUMENT_EXTS, ext)
}

/// The text after the last `.` of `name`, when that text holds no path separator.
pub(crate) fn last_extension(name: &str) -> Option<&str> {
    name.rsplit_once('.')
        .map(|(_, ext)| ext)
        .filter(|ext| !ext.contains(['/', '\\']))
}

/// The tables as JSON for the frontend (`data/media-types.json`).
pub fn media_types_json() -> String {
    fn array(list: &[&str]) -> String {
        let items: Vec<String> = list.iter().map(|ext| format!("\"{ext}\"")).collect();
        format!("[{}]", items.join(", "))
    }
    format!(
        "{{\n  \"audio\": {},\n  \"video\": {},\n  \"videoContainers\": {},\n  \"image\": {},\n  \"document\": {}\n}}\n",
        array(AUDIO_EXTS),
        array(VIDEO_EXTS),
        array(VIDEO_CONTAINER_EXTS),
        array(IMAGE_EXTS),
        array(DOCUMENT_EXTS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_ignore_case_and_a_leading_dot() {
        assert!(is_audio_ext("MP3") && is_audio_ext(".ogg") && is_audio_ext("opus"));
        assert!(is_video_ext("AVI") && is_video_ext(".mkv"));
        assert!(is_image_ext("TIF") && is_image_ext("jpeg"));
        assert!(is_document_ext("PDF"));
        assert!(!is_media_ext("png") && !is_media_ext("pdf") && !is_media_ext(""));
    }

    #[test]
    fn formats_the_picker_used_to_miss_are_now_known() {
        for ext in ["ogg", "opus", "avi", "aac", "wma", "tif"] {
            assert!(is_media_ext(ext) || is_image_ext(ext), "{ext}");
        }
    }

    #[test]
    fn webm_is_video_but_not_a_never_audio_container() {
        assert!(is_video_ext("webm"));
        assert!(!is_video_container_ext("webm"));
        assert!(is_video_container_ext("mp4") && is_video_container_ext("avi"));
    }

    #[test]
    fn the_lists_do_not_overlap_and_are_sorted_unique() {
        let all: Vec<&str> = AUDIO_EXTS
            .iter()
            .chain(VIDEO_EXTS)
            .chain(IMAGE_EXTS)
            .chain(DOCUMENT_EXTS)
            .copied()
            .collect();
        let mut unique = all.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(all.len(), unique.len(), "an extension is listed twice");
        for list in [AUDIO_EXTS, VIDEO_EXTS, VIDEO_CONTAINER_EXTS, IMAGE_EXTS] {
            let mut sorted = list.to_vec();
            sorted.sort_unstable();
            assert_eq!(list, sorted.as_slice());
        }
        for ext in VIDEO_CONTAINER_EXTS {
            assert!(VIDEO_EXTS.contains(ext), "{ext} must also be a video ext");
        }
    }

    #[test]
    fn last_extension_stops_at_a_path_separator() {
        assert_eq!(last_extension("/a/b.mp3"), Some("mp3"));
        assert_eq!(last_extension("/a.b/c"), None);
        assert_eq!(last_extension("noext"), None);
    }

    /// `data/media-types.json` must equal [`media_types_json`]. Run with
    /// `RIGHTKIT_UPDATE_GENERATED=1` to rewrite it after editing the tables.
    #[test]
    fn the_generated_frontend_json_matches_the_tables() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/media-types.json");
        let expected = media_types_json();
        if std::env::var_os("RIGHTKIT_UPDATE_GENERATED").is_some() {
            std::fs::write(&path, &expected).expect("write generated media-types.json");
            return;
        }
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let squash = |text: &str| text.split_whitespace().collect::<String>();
        assert_eq!(
            squash(&on_disk),
            squash(&expected),
            "data/media-types.json is stale; rerun with RIGHTKIT_UPDATE_GENERATED=1"
        );
    }
}
