//! How much of a string can be on screen, so drawing and measuring a string never costs more
//! than the pixels it could fill (bogusyogi/pulse#558: a 20,000-character message froze the
//! window thread because every caller shaped all of it, however little showed).
//!
//! The bound is a count of characters taken from the end that shows: the start of a left-aligned
//! run, the end of a right-aligned one. It never adds an ellipsis and never cuts a character in
//! two, so what is drawn inside `room_px` is exactly what the whole string would have drawn
//! there. The width GDI reports for what is kept is at least `room_px`, so a caller that
//! compares a width with a room reaches the same answer as with the whole string.

/// The narrowest advance any printing character is assumed to have, as a fraction of the font's
/// pixel size (Segoe UI's narrowest are about 0.2 em; a tenth leaves room for thin spaces).
/// Only zero-width characters can make the bound cut visible text short.
const MIN_ADVANCE_EM: f32 = 0.1;

/// Characters that can fill `room_px` at `size_px`, plus slack.
pub fn visible_chars(size_px: i32, room_px: f32) -> usize {
    let narrowest = size_px.max(1) as f32 * MIN_ADVANCE_EM;
    (room_px.max(0.0) / narrowest).ceil() as usize + 8
}

/// The start of `value` that can fill `room_px` at `size_px`: `value` itself when it is no
/// longer than that.
pub fn visible_prefix(value: &str, size_px: i32, room_px: f32) -> &str {
    match value.char_indices().nth(visible_chars(size_px, room_px)) {
        Some((end, _)) => &value[..end],
        None => value,
    }
}

/// The end of `value` that can fill `room_px` at `size_px` (a right-aligned run shows its
/// end): `value` itself when it is no longer than that.
pub fn visible_suffix(value: &str, size_px: i32, room_px: f32) -> &str {
    let keep = visible_chars(size_px, room_px);
    match value.char_indices().rev().nth(keep - 1) {
        Some((start, _)) if start > 0 => &value[start..],
        _ => value,
    }
}
