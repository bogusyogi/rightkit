//! `RKV1`: the lossless frame container of whole-screen capture (rightkit#443).
//!
//! The container exists because the whole-screen recorder must be lossless and must run without
//! ffmpeg: neither Media Foundation nor AVFoundation encodes a lossless codec, and an FFV1 encoder
//! is not in the dependency set. The format is deliberately small enough for the Rust writer, the
//! Swift writer (`helpers/macos`, `ScreenCapture.swift`) and the Rust reader to agree bit for bit.
//!
//! Layout, all integers little endian:
//!
//! ```text
//! header   16 bytes   "RKV1", width u32, height u32, pixel format u32 (1 = BGRA, 8 bits, packed, top-down)
//! record   16 bytes   host_ns u64, payload_len u32, flags u32 (0)
//!          payload_len bytes of tokens
//! ```
//!
//! Every frame is the byte-wise XOR of its pixels with the previous frame's (the first frame with
//! zeros), then run-length coded with two tokens: `0x00 varint(n)` is `n` zero bytes, `0x01
//! varint(n)` is `n` literal bytes that follow. A varint is LEB128, at most five bytes. The decoded
//! length of a record is exactly `width * height * 4`. A static desktop is almost all zero bytes, so
//! it costs a few bytes per frame; a frame that changes everywhere costs about its raw size.
//!
//! A capture that is killed leaves a file whose last record may be cut short. The reader treats a
//! record whose header or payload is incomplete as the end of the file, so every whole frame before
//! it is still read.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::error::{io_err, Code, RecordError, Result};

pub const MAGIC: [u8; 4] = *b"RKV1";
/// Packed BGRA, 8 bits per channel, top-down rows, no padding.
pub const FORMAT_BGRA8: u32 = 1;
pub const HEADER_BYTES: usize = 16;
const RECORD_HEADER_BYTES: usize = 16;
/// Largest width or height of a frame.
pub const MAX_SIDE: u32 = 16384;
/// A run of unchanged bytes at least this long is its own token; shorter runs stay inside a literal.
const MIN_ZERO_RUN: usize = 8;
const TOKEN_ZEROS: u8 = 0;
const TOKEN_LITERAL: u8 = 1;

fn corrupt(detail: &str) -> RecordError {
    RecordError::new(Code::Io, format!("corrupt lossless video: {detail}"))
}

fn frame_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

fn push_varint(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let low = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

/// Reads one varint of at most five bytes. Returns the value and the bytes it used.
fn read_varint(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value: usize = 0;
    for (index, byte) in bytes.iter().take(5).enumerate() {
        value |= usize::from(byte & 0x7F) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
    }
    None
}

/// Appends the tokens of `current` against `previous` (same length) to `out`.
pub fn encode_delta(previous: &[u8], current: &[u8], out: &mut Vec<u8>) {
    let length = current.len();
    let mut at = 0;
    while at < length {
        let mut run_end = at;
        while run_end < length && current[run_end] == previous[run_end] {
            run_end += 1;
        }
        if run_end > at && (run_end - at >= MIN_ZERO_RUN || run_end == length) {
            out.push(TOKEN_ZEROS);
            push_varint(out, run_end - at);
            at = run_end;
            continue;
        }
        // A literal runs until the next unchanged run long enough to be its own token, or the end.
        let start = at;
        let mut end = at;
        loop {
            while end < length && current[end] != previous[end] {
                end += 1;
            }
            if end == length {
                break;
            }
            let mut zeros_end = end;
            while zeros_end < length && current[zeros_end] == previous[zeros_end] {
                zeros_end += 1;
            }
            if zeros_end - end >= MIN_ZERO_RUN || zeros_end == length {
                break;
            }
            end = zeros_end;
        }
        out.push(TOKEN_LITERAL);
        push_varint(out, end - start);
        out.extend(
            current[start..end]
                .iter()
                .zip(&previous[start..end])
                .map(|(now, before)| now ^ before),
        );
        at = end;
    }
}

/// Rebuilds a frame of `previous.len()` bytes from `payload` into `out`.
pub fn decode_delta(payload: &[u8], previous: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let expected = previous.len();
    out.clear();
    out.reserve(expected);
    let mut at = 0;
    while at < payload.len() {
        let token = payload[at];
        at += 1;
        let (count, used) =
            read_varint(&payload[at..]).ok_or_else(|| corrupt("a run length is cut off"))?;
        at += used;
        if count == 0 || out.len() + count > expected {
            return Err(corrupt("a run is empty or runs past the frame"));
        }
        match token {
            TOKEN_ZEROS => {
                let from = out.len();
                out.extend_from_slice(&previous[from..from + count]);
            }
            TOKEN_LITERAL => {
                let bytes = payload
                    .get(at..at + count)
                    .ok_or_else(|| corrupt("literal bytes are cut off"))?;
                at += count;
                let from = out.len();
                out.extend(
                    bytes
                        .iter()
                        .zip(&previous[from..from + count])
                        .map(|(delta, before)| delta ^ before),
                );
            }
            _ => return Err(corrupt("unknown token")),
        }
    }
    if out.len() != expected {
        return Err(corrupt("a frame decodes to the wrong size"));
    }
    Ok(())
}

/// Writes frames of one size to an `RKV1` file. Each frame is flushed, so a killed capture keeps
/// every frame already written.
pub struct LosslessWriter {
    out: BufWriter<File>,
    width: u32,
    height: u32,
    previous: Vec<u8>,
    payload: Vec<u8>,
    frames: u64,
}

impl LosslessWriter {
    /// Creates `path`, which must not exist.
    pub fn create(path: &Path, width: u32, height: u32) -> Result<LosslessWriter> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(RecordError::new(
                Code::ArgInvalid,
                format!("a {width}x{height} frame is outside 1..={MAX_SIDE} per side"),
            ));
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::AlreadyExists {
                    RecordError::new(
                        Code::OutExists,
                        format!("{} already exists", path.display()),
                    )
                } else {
                    io_err(&format!("create {}", path.display()), &e)
                }
            })?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        let mut header = Vec::with_capacity(HEADER_BYTES);
        header.extend_from_slice(&MAGIC);
        header.extend_from_slice(&width.to_le_bytes());
        header.extend_from_slice(&height.to_le_bytes());
        header.extend_from_slice(&FORMAT_BGRA8.to_le_bytes());
        out.write_all(&header)
            .and_then(|()| out.flush())
            .map_err(|e| io_err("write the lossless video header", &e))?;
        Ok(LosslessWriter {
            out,
            width,
            height,
            previous: vec![0; frame_bytes(width, height)],
            payload: Vec::new(),
            frames: 0,
        })
    }

    /// Appends one frame of packed BGRA pixels taken at `host_ns`.
    pub fn write_frame(&mut self, host_ns: u64, bgra: &[u8]) -> Result<()> {
        if bgra.len() != self.previous.len() {
            return Err(RecordError::new(
                Code::ArgInvalid,
                format!(
                    "a {}x{} frame is {} bytes, got {}",
                    self.width,
                    self.height,
                    self.previous.len(),
                    bgra.len()
                ),
            ));
        }
        self.payload.clear();
        encode_delta(&self.previous, bgra, &mut self.payload);
        let length = u32::try_from(self.payload.len()).map_err(|_| {
            RecordError::new(Code::Io, "an encoded frame does not fit a 32-bit length")
        })?;
        let mut header = [0u8; RECORD_HEADER_BYTES];
        header[0..8].copy_from_slice(&host_ns.to_le_bytes());
        header[8..12].copy_from_slice(&length.to_le_bytes());
        self.out
            .write_all(&header)
            .and_then(|()| self.out.write_all(&self.payload))
            .and_then(|()| self.out.flush())
            .map_err(|e| io_err("write a lossless frame", &e))?;
        self.previous.copy_from_slice(bgra);
        self.frames += 1;
        Ok(())
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Flushes and syncs the file; returns the frames written.
    pub fn finish(mut self) -> Result<u64> {
        self.out
            .flush()
            .and_then(|()| self.out.get_ref().sync_all())
            .map_err(|e| io_err("finish the lossless video", &e))?;
        Ok(self.frames)
    }
}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub index: u64,
    pub host_ns: u64,
    /// Packed BGRA, `width * height * 4` bytes.
    pub bgra: Vec<u8>,
}

/// Reads frames back from an `RKV1` file.
pub struct LosslessReader {
    input: BufReader<File>,
    width: u32,
    height: u32,
    previous: Vec<u8>,
    payload: Vec<u8>,
    index: u64,
    done: bool,
}

/// Reads up to `buffer.len()` bytes, stopping early only at the end of the file.
fn read_full(input: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match input.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

impl LosslessReader {
    pub fn open(path: &Path) -> Result<LosslessReader> {
        let file = File::open(path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                RecordError::new(Code::NotFound, format!("{} not found", path.display()))
            } else {
                io_err(&format!("open {}", path.display()), &e)
            }
        })?;
        let mut input = BufReader::with_capacity(1 << 20, file);
        let mut header = [0u8; HEADER_BYTES];
        let read = read_full(&mut input, &mut header)
            .map_err(|e| io_err("read the lossless video header", &e))?;
        if read < HEADER_BYTES || header[0..4] != MAGIC {
            return Err(corrupt("not an RKV1 file"));
        }
        let word = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        let (width, height, format) = (word(4), word(8), word(12));
        if format != FORMAT_BGRA8 {
            return Err(corrupt("unknown pixel format"));
        }
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(corrupt("frame size is out of range"));
        }
        Ok(LosslessReader {
            input,
            width,
            height,
            previous: vec![0; frame_bytes(width, height)],
            payload: Vec::new(),
            index: 0,
            done: false,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The next frame, or `None` at the end of the file. A cut-off last record is the end.
    pub fn next_frame(&mut self) -> Result<Option<RawFrame>> {
        if self.done {
            return Ok(None);
        }
        let mut header = [0u8; RECORD_HEADER_BYTES];
        let read = read_full(&mut self.input, &mut header)
            .map_err(|e| io_err("read a lossless frame header", &e))?;
        if read < RECORD_HEADER_BYTES {
            self.done = true;
            return Ok(None);
        }
        let mut word = [0u8; 8];
        word.copy_from_slice(&header[0..8]);
        let host_ns = u64::from_le_bytes(word);
        let length = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
        let ceiling = frame_bytes(self.width, self.height) * 2 + 64;
        if length > ceiling {
            return Err(corrupt("a record is larger than any frame can encode to"));
        }
        self.payload.resize(length, 0);
        let read = read_full(&mut self.input, &mut self.payload)
            .map_err(|e| io_err("read a lossless frame", &e))?;
        if read < length {
            self.done = true;
            return Ok(None);
        }
        let mut bgra = Vec::new();
        decode_delta(&self.payload, &self.previous, &mut bgra)?;
        self.previous.clone_from(&bgra);
        let frame = RawFrame {
            index: self.index,
            host_ns,
            bgra,
        };
        self.index += 1;
        Ok(Some(frame))
    }
}
