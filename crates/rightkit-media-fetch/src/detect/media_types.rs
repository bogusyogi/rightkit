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
