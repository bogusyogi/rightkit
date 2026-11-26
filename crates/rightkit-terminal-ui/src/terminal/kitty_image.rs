//! Kitty graphics protocol output for a PNG file (B-cximages). Pure byte building: whether the
//! terminal speaks the protocol is [`super::capability::detect_image_protocol`]'s call, and where
//! on screen the picture goes is the host's.
//!
//! One transmit-and-display command (`a=T`, `f=100` PNG) split into 4096-byte base64 chunks as the
//! protocol requires (`m=1` on every chunk but the last), scaled into a fixed block of cells
//! (`c`, `r`), with the cursor left where it was (`C=1`) and no reply asked for (`q=2`).

/// Largest PNG shown (bytes): the sequence is written whole to the terminal.
pub const MAX_PNG_BYTES: usize = 1024 * 1024;
/// Widest picture, in cells, and the tallest block of rows it may take.
pub const MAX_COLS: u16 = 40;
pub const MAX_ROWS: u16 = 12;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const CHUNK: usize = 4096;

/// Width and height in pixels from the IHDR chunk, when `bytes` is a PNG.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || bytes[..8] != PNG_SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// The block of cells `(cols, rows)` the picture takes: at most `max_cols` wide and
/// [`MAX_ROWS`] tall, keeping its proportions on cells about twice as tall as wide.
pub fn cell_box(size: (u32, u32), max_cols: u16) -> (u16, u16) {
    let (width, height) = (f64::from(size.0), f64::from(size.1));
    let mut cols = f64::from(max_cols.clamp(1, MAX_COLS));
    let mut rows = (cols * height / width / 2.0).ceil().max(1.0);
    if rows > f64::from(MAX_ROWS) {
        rows = f64::from(MAX_ROWS);
        cols = (rows * 2.0 * width / height).floor().max(1.0);
    }
    (cols as u16, rows as u16)
}

/// The escape sequences that display `png` in a `cols` by `rows` block at the cursor. None when
/// `png` is not a PNG or is over [`MAX_PNG_BYTES`].
pub fn display_png(png: &[u8], cols: u16, rows: u16) -> Option<Vec<u8>> {
    if png.len() > MAX_PNG_BYTES || png_size(png).is_none() {
        return None;
    }
    let encoded = rightkit_crypto::base64_encode(png);
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(CHUNK).collect();
    let mut out = Vec::with_capacity(encoded.len() + chunks.len() * 16 + 64);
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        out.extend_from_slice(b"\x1b_G");
        if index == 0 {
            out.extend_from_slice(
                format!("a=T,f=100,t=d,c={cols},r={rows},C=1,q=2,m={more};").as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("m={more};").as_bytes());
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    Some(out)
}
