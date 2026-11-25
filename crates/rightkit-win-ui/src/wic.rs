//! Windows Imaging Component file conversion.

use rightkit_fs::{AtomicWriteOptions, FsError, OwnedTemp, ReplaceMode};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{E_FAIL, ERROR_FILE_EXISTS, GENERIC_READ};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, IWICBitmapFrameEncode, IWICImagingFactory,
    WICBitmapEncoderNoCache, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IStream};
use windows::core::{Error, HRESULT, IUnknown, Interface, PCWSTR};

/// Writes the first frame of `source` to a new PNG at `target`.
///
/// The calling thread must have initialised COM. The target appears only after
/// the encoder closes successfully. An existing file is preserved. On failure
/// the callback receives the codec HRESULT or the storage operation's Win32 code.
pub fn reencode_png(source: &Path, target: &Path, on_error: impl FnOnce(&Error)) -> bool {
    let written = (|| -> Result<(), Error> {
        let stage = OwnedTemp::create(target, "rightkit-win-ui:wic").map_err(storage_error)?;
        encode_first_frame(source, stage.path())?;
        stage
            .publish(AtomicWriteOptions {
                replace: ReplaceMode::NoReplace,
                ..AtomicWriteOptions::default()
            })
            .map_err(storage_error)
    })();
    match written {
        Ok(()) => true,
        Err(error) => {
            on_error(&error);
            false
        }
    }
}

fn storage_error(error: FsError) -> Error {
    let code = match error {
        FsError::AlreadyExists(_) => HRESULT::from_win32(ERROR_FILE_EXISTS.0),
        FsError::Io { source, .. }
        | FsError::DirSyncFailed { source, .. }
        | FsError::ReplacedButDirSyncFailed { source, .. }
        | FsError::ReplacedButPostStepFailed { source, .. } => source
            .raw_os_error()
            .map_or(E_FAIL, |code| HRESULT::from_win32(code as u32)),
        _ => E_FAIL,
    };
    Error::from_hresult(code)
}

fn encode_first_frame(source: &Path, stage: &Path) -> Result<(), Error> {
    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (from, to) = (wide(source), wide(stage));
    // SAFETY: the caller's thread has initialised COM; every interface is
    // released on drop and both names outlive the calls.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(
            &CLSID_WICImagingFactory,
            None::<&IUnknown>,
            CLSCTX_INPROC_SERVER,
        )?;
        let decoder = factory.CreateDecoderFromFilename(
            PCWSTR(from.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        let picture = decoder.GetFrame(0)?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromFilename(PCWSTR(to.as_ptr()), 0x4000_0000)?; // GENERIC_WRITE
        let istream: IStream = stream.cast()?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&istream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
        frame.Initialize(None::<&IPropertyBag2>)?;
        frame.WriteSource(&picture, std::ptr::null())?;
        frame.Commit()?;
        encoder.Commit()
    }
}
