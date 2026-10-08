//! 32-bit top-down BMP header. Frames are written as BMP so they open in any viewer.

pub const HEADER_LEN: usize = 54;

pub fn header(width: u32, height: u32) -> [u8; HEADER_LEN] {
    let data_len = width * height * 4;
    let file_len = HEADER_LEN as u32 + data_len;
    let mut h = [0u8; HEADER_LEN];
    h[0..2].copy_from_slice(b"BM");
    h[2..6].copy_from_slice(&file_len.to_le_bytes());
    h[10..14].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    h[14..18].copy_from_slice(&40u32.to_le_bytes());
    h[18..22].copy_from_slice(&(width as i32).to_le_bytes());
    // Negative height marks the rows as top-down.
    h[22..26].copy_from_slice(&(-(height as i32)).to_le_bytes());
    h[26..28].copy_from_slice(&1u16.to_le_bytes());
    h[28..30].copy_from_slice(&32u16.to_le_bytes());
    // Compression (BI_RGB = 0) at 30..34 stays zero.
    h[34..38].copy_from_slice(&data_len.to_le_bytes());
    h[38..42].copy_from_slice(&2835u32.to_le_bytes());
    h[42..46].copy_from_slice(&2835u32.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_describes_top_down_bgra() {
        let h = header(1964, 1282);
        assert_eq!(&h[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(h[2..6].try_into().unwrap()), 54 + 1964 * 1282 * 4);
        assert_eq!(u32::from_le_bytes(h[10..14].try_into().unwrap()), 54);
        assert_eq!(i32::from_le_bytes(h[18..22].try_into().unwrap()), 1964);
        assert_eq!(i32::from_le_bytes(h[22..26].try_into().unwrap()), -1282);
        assert_eq!(u16::from_le_bytes(h[28..30].try_into().unwrap()), 32);
        assert_eq!(u32::from_le_bytes(h[30..34].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(h[34..38].try_into().unwrap()), 1964 * 1282 * 4);
    }
}
