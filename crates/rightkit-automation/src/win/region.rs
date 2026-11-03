//! Screen-region capture: a rectangle of the virtual screen, blitted from the screen DC
//! (`BitBlt` with `CAPTUREBLT`, so layered windows are included). Windows that opted out of
//! capture (`WDA_EXCLUDEFROMCAPTURE`) are left out by the compositor. The cross-platform
//! surface and the safety checks are in [`crate::screen`].

use crate::model::*;
use windows::Win32::Graphics::Gdi::*;

pub fn capture_region(r: Rect, path: &std::path::Path) -> Result<()> {
    let (x, y) = (r.x.round() as i32, r.y.round() as i32);
    let (w, h) = (r.w.round() as i32, r.h.round() as i32);
    if w <= 0 || h <= 0 {
        return Err(Error::Invalid("capture region is empty".into()));
    }
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err(Error::Platform(0, "GetDC"));
        }
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let blit = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok();
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut px = vec![0u8; (w as usize) * (h as usize) * 4];
        let rows = GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            Some(px.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !blit || rows == 0 {
            return Err(Error::Unsupported("BitBlt from the screen failed"));
        }
        for p in px.as_chunks_mut::<4>().0 {
            p.swap(0, 2); // BGRA -> RGBA
            p[3] = 255;
        }
        let png = crate::png::encode_rgba(w as u32, h as u32, &px)
            .ok_or(Error::Unsupported("png encode"))?;
        std::fs::write(path, png).map_err(|_| Error::Platform(0, "write png"))
    }
}
