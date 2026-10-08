//! Media Foundation output: an MP4 written through IMFSinkWriter with an H.264
//! or HEVC encoder MFT. The worker thread owns the writer. Frames arrive as
//! packed BGRA and are converted straight into a locked NV12 sample buffer.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use ::windows::core::PCWSTR;
use ::windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFByteStream, IMFMediaType, IMFSinkWriter, MFCreateAttributes, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Video, MFNominalRange_16_235,
    MFShutdown, MFStartup, MFTEnumEx, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_FRIENDLY_NAME_Attribute,
    MFT_REGISTER_TYPE_INFO, MFSTARTUP_FULL, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, MFVideoTransferMatrix_BT709, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_VERSION,
};
use ::windows::Win32::System::Com::CoTaskMemFree;
use serde_json::{json, Value};

use crate::codec::{bitrate_bps, even_dimension, bgra_to_nv12, nv12_len, sample_duration_100ns, Codec};
use crate::error::{Code, RecordError};

/// MFStartup for the life of a run. Dropping it calls MFShutdown, so every
/// return path, including errors, pairs the two calls.
pub struct Startup;

impl Startup {
    pub fn start() -> Result<Self, RecordError> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
            .map_err(|e| RecordError::new(Code::Unsupported, format!("MFStartup failed: {e}")))?;
        Ok(Startup)
    }
}

impl Drop for Startup {
    fn drop(&mut self) {
        let _ = unsafe { MFShutdown() };
    }
}

/// The encoder the run was configured for. `hardware` comes from the enumeration
/// that ranked it first, not from a query on the live sink writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoder {
    pub name: String,
    pub hardware: bool,
}

impl Encoder {
    pub fn to_json(&self) -> Value {
        json!({ "kind": if self.hardware { "hardware" } else { "software" }, "name": self.name })
    }
}

/// MP4 sink for one capture. Create on the owning thread, then move it to the
/// worker. `finalize` writes the moov atom. Drop finalizes too, so an
/// interrupted run still leaves a playable file.
pub struct Mp4Writer {
    writer: IMFSinkWriter,
    stream: u32,
    width: u32,
    height: u32,
    fps: u32,
    encoder: Encoder,
    finalized: bool,
}

// The sink writer is created on the MTA main thread and afterwards touched only
// by the worker, which is the only thread that uses it.
unsafe impl Send for Mp4Writer {}

impl Mp4Writer {
    /// `native_width` and `native_height` come from the capture item. The encoded
    /// frame is the top-left even-rounded region of them.
    pub fn create(path: &Path, codec: Codec, native_width: u32, native_height: u32, fps: u32) -> Result<Self, RecordError> {
        let subtype = match codec {
            Codec::H264 => MFVideoFormat_H264,
            Codec::Hevc => MFVideoFormat_HEVC,
        };
        let width = even_dimension(native_width)
            .ok_or_else(|| RecordError::new(Code::Helper, format!("frame width {native_width} is too small")))?;
        let height = even_dimension(native_height)
            .ok_or_else(|| RecordError::new(Code::Helper, format!("frame height {native_height} is too small")))?;
        let encoder = select_encoder(codec, subtype)?;

        let mut attributes: Option<IMFAttributes> = None;
        unsafe { MFCreateAttributes(&mut attributes, 1) }.map_err(|e| setup_error("MFCreateAttributes", e))?;
        let attributes = attributes.ok_or_else(|| setup_error("MFCreateAttributes", "no attribute store"))?;
        unsafe { attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1) }
            .map_err(|e| setup_error("enable hardware transforms", e))?;

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let writer = unsafe { MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None::<&IMFByteStream>, &attributes) }
            .map_err(|e| RecordError::new(Code::Io, format!("MFCreateSinkWriterFromURL failed: {e}")))?;

        let output = unsafe { MFCreateMediaType() }.map_err(|e| setup_error("MFCreateMediaType", e))?;
        set_frame_format(&output, subtype, width, height, fps)?;
        unsafe { output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps(width, height, fps)) }
            .map_err(|e| setup_error("MF_MT_AVG_BITRATE", e))?;
        let stream = unsafe { writer.AddStream(&output) }.map_err(|e| setup_error("AddStream", e))?;

        let input = unsafe { MFCreateMediaType() }.map_err(|e| setup_error("MFCreateMediaType", e))?;
        set_frame_format(&input, MFVideoFormat_NV12, width, height, fps)?;
        unsafe { input.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32) }
            .map_err(|e| setup_error("MF_MT_YUV_MATRIX", e))?;
        unsafe { input.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32) }
            .map_err(|e| setup_error("MF_MT_VIDEO_NOMINAL_RANGE", e))?;
        unsafe { writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>) }
            .map_err(|e| setup_error("SetInputMediaType", e))?;
        unsafe { writer.BeginWriting() }.map_err(|e| setup_error("BeginWriting", e))?;

        Ok(Self { writer, stream, width, height, fps, encoder, finalized: false })
    }

    pub fn encoder(&self) -> &Encoder {
        &self.encoder
    }

    /// Encoded frame size after even rounding.
    pub fn frame_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Converts one packed BGRA frame (`src_width` pixels per row) and writes it
    /// at `sample_time` (100 ns units from the first kept frame).
    pub fn write_frame(&mut self, packed_bgra: &[u8], src_width: usize, sample_time: i64) -> Result<(), RecordError> {
        let (width, height) = (self.width as usize, self.height as usize);
        let len = nv12_len(width, height);
        let buffer = unsafe { MFCreateMemoryBuffer(len as u32) }.map_err(|e| capture_failure("MFCreateMemoryBuffer", e))?;
        let mut data: *mut u8 = ptr::null_mut();
        unsafe { buffer.Lock(&mut data, None, None) }.map_err(|e| capture_failure("IMFMediaBuffer::Lock", e))?;
        // Safety: Lock returned a writable region of `len` bytes (MFCreateMemoryBuffer(len)).
        let converted = {
            let dst = unsafe { std::slice::from_raw_parts_mut(data, len) };
            bgra_to_nv12(packed_bgra, src_width * 4, width, height, dst)
        };
        unsafe { buffer.Unlock() }.map_err(|e| capture_failure("IMFMediaBuffer::Unlock", e))?;
        converted.map_err(|e| RecordError::new(Code::Helper, format!("NV12 conversion failed: {e}")))?;
        unsafe { buffer.SetCurrentLength(len as u32) }.map_err(|e| capture_failure("SetCurrentLength", e))?;

        let sample = unsafe { MFCreateSample() }.map_err(|e| capture_failure("MFCreateSample", e))?;
        unsafe { sample.AddBuffer(&buffer) }.map_err(|e| capture_failure("AddBuffer", e))?;
        unsafe { sample.SetSampleTime(sample_time) }.map_err(|e| capture_failure("SetSampleTime", e))?;
        unsafe { sample.SetSampleDuration(sample_duration_100ns(self.fps)) }
            .map_err(|e| capture_failure("SetSampleDuration", e))?;
        unsafe { self.writer.WriteSample(self.stream, &sample) }.map_err(|e| capture_failure("WriteSample", e))
    }

    /// Writes the container trailer. Idempotent.
    pub fn finalize(&mut self) -> Result<(), RecordError> {
        if self.finalized {
            return Ok(());
        }
        self.finalized = true;
        unsafe { self.writer.Finalize() }.map_err(|e| RecordError::new(Code::Io, format!("IMFSinkWriter::Finalize failed: {e}")))
    }
}

impl Drop for Mp4Writer {
    fn drop(&mut self) {
        let _ = self.finalize();
    }
}

/// Picks the encoder that will be reported. Hardware is ranked first, then
/// software. HEVC with no registered encoder is an error, never a fallback.
fn select_encoder(codec: Codec, subtype: ::windows::core::GUID) -> Result<Encoder, RecordError> {
    let hardware = enumerate(subtype, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER)?;
    if let Some(name) = hardware.into_iter().next() {
        return Ok(Encoder { name, hardware: true });
    }
    let software = enumerate(subtype, MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER)?;
    match software.into_iter().next() {
        Some(name) => Ok(Encoder { name, hardware: false }),
        None => Err(RecordError::new(
            Code::Unsupported,
            format!("no {} encoder MFT is registered on this host", codec.as_str()),
        )),
    }
}

/// Friendly names of the video encoder MFTs that take NV12 and produce `output`.
fn enumerate(output: ::windows::core::GUID, flags: MFT_ENUM_FLAG) -> Result<Vec<String>, RecordError> {
    let input_info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output_info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: output };
    let mut list: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&input_info as *const MFT_REGISTER_TYPE_INFO),
            Some(&output_info as *const MFT_REGISTER_TYPE_INFO),
            &mut list,
            &mut count,
        )
    }
    .map_err(|e| RecordError::new(Code::Unsupported, format!("MFTEnumEx failed: {e}")))?;

    let mut names = Vec::new();
    if !list.is_null() {
        // Safety: MFTEnumEx returned `count` slots in a CoTaskMem block. Each slot
        // is taken (and released) exactly once, then the block is freed.
        let slots = unsafe { std::slice::from_raw_parts_mut(list, count as usize) };
        for slot in slots.iter_mut() {
            if let Some(activate) = slot.take() {
                names.push(friendly_name(&activate));
            }
        }
        unsafe { CoTaskMemFree(Some(list as *const c_void)) };
    }
    Ok(names)
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut buffer = [0u16; 256];
    let mut len = 0u32;
    match unsafe { activate.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buffer, Some(&mut len as *mut u32)) } {
        Ok(()) => String::from_utf16_lossy(&buffer[..(len as usize).min(buffer.len())]),
        Err(_) => "unnamed encoder".to_string(),
    }
}

/// Sets the attributes shared by the input and output media types.
fn set_frame_format(
    media_type: &IMFMediaType,
    subtype: ::windows::core::GUID,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<(), RecordError> {
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| setup_error("MF_MT_MAJOR_TYPE", e))?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &subtype).map_err(|e| setup_error("MF_MT_SUBTYPE", e))?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_pair(width, height)).map_err(|e| setup_error("MF_MT_FRAME_SIZE", e))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack_pair(fps, 1)).map_err(|e| setup_error("MF_MT_FRAME_RATE", e))?;
        media_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_pair(1, 1))
            .map_err(|e| setup_error("MF_MT_PIXEL_ASPECT_RATIO", e))?;
        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| setup_error("MF_MT_INTERLACE_MODE", e))?;
    }
    Ok(())
}

/// Media Foundation packs a (high, low) 32-bit pair into one UINT64.
fn pack_pair(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Encoder setup failures mean the requested codec cannot be produced here.
fn setup_error(step: &str, error: impl std::fmt::Display) -> RecordError {
    RecordError::new(Code::Unsupported, format!("{step} failed: {error}"))
}

fn capture_failure(step: &str, error: impl std::fmt::Display) -> RecordError {
    RecordError::new(Code::Helper, format!("{step} failed: {error}"))
}
