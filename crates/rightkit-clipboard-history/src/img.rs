//! Pictures for the clipboard history: the conversions between what the clipboard holds (a
//! registered PNG, or a DIB) and what the store keeps (a PNG), the 320-pixel thumbnail of the
//! spec's section 3.2, and the pixels the strip draws. The picture codec is WIC, the same the
//! screenshots use. Clipboard payloads are converted in memory. Call `init_com` on the calling
//! thread.

use crate::model as clip;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{E_FAIL, GENERIC_READ};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    GUID_WICPixelFormat32bppPBGRA, IWICBitmapDecoder, IWICBitmapFrameEncode, IWICImagingFactory,
    IWICPalette, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IStream,
    STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{Error, IUnknown, PCWSTR};

/// The longest side of a card's thumbnail (spec 3.2).
pub const THUMB_SIDE: u32 = 320;
/// Pictures larger than this many pixels are offered to other apps as PNG only.
const DIB_PIXELS_MAX: u64 = 16 * 1024 * 1024;

/// A picture as straight-alpha BGRA bytes, rows top to bottom.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Makes this thread able to use WIC (a second call only adds a count).
pub fn init_com() {
    // SAFETY: COM initialisation of the calling thread; the thread keeps its apartment for
    // its whole life, as the launcher's threads do.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// The width and height a PNG's header gives, without decoding it.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((be(16), be(20)))
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Reads a picture file (PNG or BMP) as BGRA. With `max_side`, a picture longer than that is
/// scaled down (Fant) while it is read, so a huge image is never held whole.
pub fn decode_file(path: &Path, max_side: Option<u32>) -> Option<Bitmap> {
    let name = wide(path);
    // SAFETY: the terminated name outlives this WIC call.
    decode_first(max_side, |factory| unsafe {
        factory.CreateDecoderFromFilename(
            PCWSTR(name.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )
    })
}

/// Decodes a stored clipboard image without writing a plaintext copy to disk.
pub fn decode_png(png: &[u8], max_side: Option<u32>) -> Option<Bitmap> {
    let (width, height) = png_size(png)?;
    if png.len() > clip::IMAGE_MAX || width > clip::IMAGE_SIDE_MAX || height > clip::IMAGE_SIDE_MAX
    {
        return None;
    }
    if max_side.is_none() && u64::from(width) * u64::from(height) > DIB_PIXELS_MAX {
        return None;
    }
    // SAFETY: the calling thread initialised COM. The stream copies the source bytes and
    // remains alive throughout decoding.
    let stream = unsafe { SHCreateMemStream(Some(png)) }?;
    decode_first(max_side, |factory| unsafe {
        factory.CreateDecoderFromStream(&stream, None, WICDecodeMetadataCacheOnDemand)
    })
}

pub(crate) fn dibv5_from_png(png: &[u8]) -> Option<Vec<u8>> {
    dibv5(&decode_png(png, None)?)
}

fn decode_first(
    max_side: Option<u32>,
    open: impl FnOnce(&IWICImagingFactory) -> Result<IWICBitmapDecoder, Error>,
) -> Option<Bitmap> {
    // SAFETY: COM is initialised on this thread. Interfaces are released on drop and each
    // pixel buffer is exactly `stride * height` long.
    unsafe {
        let run = || -> Result<Bitmap, Error> {
            let factory: IWICImagingFactory = CoCreateInstance(
                &CLSID_WICImagingFactory,
                None::<&IUnknown>,
                CLSCTX_INPROC_SERVER,
            )?;
            let decoder = open(&factory)?;
            let frame = decoder.GetFrame(0)?;
            let converter = factory.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let (mut width, mut height) = (0u32, 0u32);
            converter.GetSize(&mut width, &mut height)?;
            if width == 0 || height == 0 {
                return Err(Error::from(E_FAIL));
            }
            let longest = width.max(height);
            let (w, h, buffer) = match max_side.filter(|side| longest > *side) {
                Some(side) => {
                    let scale = f64::from(side) / f64::from(longest);
                    let (w, h) = (
                        ((f64::from(width) * scale).round() as u32).max(1),
                        ((f64::from(height) * scale).round() as u32).max(1),
                    );
                    let scaler = factory.CreateBitmapScaler()?;
                    scaler.Initialize(&converter, w, h, WICBitmapInterpolationModeFant)?;
                    let mut buffer = vec![0u8; w as usize * h as usize * 4];
                    scaler.CopyPixels(std::ptr::null(), w * 4, &mut buffer)?;
                    (w, h, buffer)
                }
                None => {
                    let mut buffer = vec![0u8; width as usize * height as usize * 4];
                    converter.CopyPixels(std::ptr::null(), width * 4, &mut buffer)?;
                    (width, height, buffer)
                }
            };
            Ok(Bitmap {
                width: w,
                height: h,
                bgra: buffer,
            })
        };
        run().ok()
    }
}

/// Reads any picture file WIC has a codec for as a `side` x `side` premultiplied `0xAARRGGBB`
/// square, scaled to fill and centre-cropped (the Mac tile's `scaledToFill`).
pub fn decode_cover(path: &Path, side: u32) -> Option<Vec<u32>> {
    let name = wide(path);
    init_com();
    // SAFETY: COM is initialised just above; every interface is released on drop, `name`
    // outlives the calls and the pixel buffer is exactly `side * side * 4` bytes long.
    unsafe {
        let run = || -> Result<Vec<u32>, Error> {
            let factory: IWICImagingFactory = CoCreateInstance(
                &CLSID_WICImagingFactory,
                None::<&IUnknown>,
                CLSCTX_INPROC_SERVER,
            )?;
            let decoder = factory.CreateDecoderFromFilename(
                PCWSTR(name.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )?;
            let frame = decoder.GetFrame(0)?;
            let converter = factory.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let (mut width, mut height) = (0u32, 0u32);
            converter.GetSize(&mut width, &mut height)?;
            if width == 0 || height == 0 || side == 0 {
                return Err(Error::from(E_FAIL));
            }
            let scale = f64::from(side) / f64::from(width.min(height));
            let w = ((f64::from(width) * scale).round() as u32).max(side);
            let h = ((f64::from(height) * scale).round() as u32).max(side);
            let scaler = factory.CreateBitmapScaler()?;
            scaler.Initialize(&converter, w, h, WICBitmapInterpolationModeFant)?;
            let crop = WICRect {
                X: ((w - side) / 2) as i32,
                Y: ((h - side) / 2) as i32,
                Width: side as i32,
                Height: side as i32,
            };
            let mut buffer = vec![0u8; side as usize * side as usize * 4];
            scaler.CopyPixels(&crop, side * 4, &mut buffer)?;
            Ok(buffer
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| u32::from_le_bytes(*p))
                .collect())
        };
        run().ok()
    }
}

/// The picture as PNG bytes.
pub fn encode_png(bitmap: &Bitmap) -> Option<Vec<u8>> {
    // SAFETY: the frame is initialized and the slice is readable for the call.
    encode_png_with(|_, frame| unsafe {
        frame.SetSize(bitmap.width, bitmap.height)?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format)?;
        frame.WritePixels(bitmap.height, bitmap.width * 4, &bitmap.bgra)
    })
    .ok()
}

fn encode_png_with(
    write: impl FnOnce(&IWICImagingFactory, &IWICBitmapFrameEncode) -> Result<(), Error>,
) -> Result<Vec<u8>, Error> {
    // SAFETY: the calling thread initialised COM. Interfaces and initialized buffers
    // remain alive through every call. Stat provides the stream's logical byte count.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(
            &CLSID_WICImagingFactory,
            None::<&IUnknown>,
            CLSCTX_INPROC_SERVER,
        )?;
        let stream: IStream = SHCreateMemStream(None).ok_or_else(|| Error::from(E_FAIL))?;
        {
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame = None;
            encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
            let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
            frame.Initialize(None::<&IPropertyBag2>)?;
            write(&factory, &frame)?;
            frame.Commit()?;
            encoder.Commit()?;
        }
        let mut state = STATSTG::default();
        stream.Stat(&mut state, STATFLAG_NONAME)?;
        let size = usize::try_from(state.cbSize)
            .ok()
            .filter(|&size| size > 0 && size <= clip::IMAGE_MAX)
            .ok_or_else(|| Error::from(E_FAIL))?;
        let mut bytes = vec![0; size];
        let mut read = 0;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        stream
            .Read(bytes.as_mut_ptr().cast(), size as u32, Some(&mut read))
            .ok()?;
        if read as usize != size {
            return Err(Error::from(E_FAIL));
        }
        Ok(bytes)
    }
}

/// A card's thumbnail: the PNG file, scaled so its longest side is `THUMB_SIDE`, as PNG.
pub fn thumbnail(png: &Path) -> Option<Vec<u8>> {
    let small = decode_file(png, Some(THUMB_SIDE))?;
    encode_png(&small)
}

pub(crate) fn thumbnail_png(png: &[u8]) -> Option<Vec<u8>> {
    encode_png(&decode_png(png, Some(THUMB_SIDE))?)
}

/// A clipboard DIB (a `BITMAPINFOHEADER` or newer and the pixels) as PNG bytes.
pub fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    let bmp = bmp_file(dib)?;
    u32::try_from(bmp.len()).ok()?;
    // SAFETY: COM is initialised, the source bytes are copied into the stream and
    // the stream remains alive until the encoder finishes.
    let converted = encode_png_with(|factory, output| unsafe {
        let input = SHCreateMemStream(Some(&bmp)).ok_or_else(|| Error::from(E_FAIL))?;
        let decoder =
            factory.CreateDecoderFromStream(&input, None, WICDecodeMetadataCacheOnDemand)?;
        let picture = decoder.GetFrame(0)?;
        let (mut width, mut height) = (0, 0);
        picture.GetSize(&mut width, &mut height)?;
        if width == 0
            || height == 0
            || width > clip::IMAGE_SIDE_MAX
            || height > clip::IMAGE_SIDE_MAX
        {
            return Err(Error::from(E_FAIL));
        }
        output.WriteSource(&picture, std::ptr::null())
    });
    match converted {
        Ok(png) => Some(png),
        Err(error) => {
            crate::host::log_png_error(&error);
            None
        }
    }
}

/// A DIB with the 14-byte file header put in front, so WIC can read it.
fn bmp_file(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |at: usize| {
        dib.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let header = u32_at(0)? as usize;
    if !(40..=dib.len()).contains(&header) {
        return None;
    }
    let bits = dib.get(14..16).map(|b| u16::from_le_bytes([b[0], b[1]]))? as u32;
    let compression = u32_at(16)?;
    let used = u32_at(32)?;
    let colours: usize = if used > 0 {
        used as usize
    } else if bits <= 8 {
        1usize << bits
    } else {
        0
    };
    // BI_BITFIELDS (3) and BI_ALPHABITFIELDS (6) put the channel masks after a 40-byte header.
    let masks = if header == 40 {
        match compression {
            3 => 12,
            6 => 16,
            _ => 0,
        }
    } else {
        0
    };
    let offset = 14 + header + masks + colours * 4;
    if offset > 14 + dib.len() {
        return None;
    }
    let total = 14 + dib.len();
    let mut file = Vec::with_capacity(total);
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&(total as u32).to_le_bytes());
    file.extend_from_slice(&[0, 0, 0, 0]);
    file.extend_from_slice(&(offset as u32).to_le_bytes());
    file.extend_from_slice(dib);
    Some(file)
}

/// The picture as a `CF_DIBV5` block (a 124-byte `BITMAPV5HEADER`, then bottom-up BGRA with
/// straight alpha), for apps that do not read the registered PNG. `None` for a very large
/// picture, which then goes as PNG alone.
pub fn dibv5(bitmap: &Bitmap) -> Option<Vec<u8>> {
    let (width, height) = (bitmap.width as usize, bitmap.height as usize);
    if (width as u64) * (height as u64) > DIB_PIXELS_MAX {
        return None;
    }
    let image = width * height * 4;
    let mut out = Vec::with_capacity(124 + image);
    let mut word = |value: u32| out.extend_from_slice(&value.to_le_bytes());
    word(124); // bV5Size
    word(bitmap.width); // bV5Width
    word(bitmap.height); // bV5Height: positive, rows bottom up
    word(1 | (32 << 16)); // planes, bit count
    word(3); // BI_BITFIELDS
    word(image as u32);
    word(0); // x pixels per metre
    word(0); // y pixels per metre
    word(0); // colours used
    word(0); // colours important
    word(0x00FF_0000); // red mask
    word(0x0000_FF00); // green mask
    word(0x0000_00FF); // blue mask
    word(0xFF00_0000); // alpha mask
    word(0x7352_4742); // LCS_sRGB
    for _ in 0..9 {
        word(0); // the colour endpoints (36 bytes)
    }
    for _ in 0..3 {
        word(0); // gamma red, green, blue
    }
    word(4); // LCS_GM_IMAGES
    word(0); // profile data
    word(0); // profile size
    word(0); // reserved
    for row in (0..height).rev() {
        out.extend_from_slice(&bitmap.bgra[row * width * 4..(row + 1) * width * 4]);
    }
    Some(out)
}

/// The pixels as the canvas takes them: premultiplied `0xAARRGGBB`.
pub fn premultiplied(bitmap: &Bitmap) -> Vec<u32> {
    bitmap
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let alpha = u32::from(p[3]);
            let scale = |c: u8| (u32::from(c) * alpha + 127) / 255;
            (alpha << 24) | (scale(p[2]) << 16) | (scale(p[1]) << 8) | scale(p[0])
        })
        .collect()
}
