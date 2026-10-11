//! Windows transfer primitives: one overlapped `ReadFile`/`WriteFile` per
//! attempt, waited on in `WaitForSingleObject` slices bounded by the deadline.
//!
//! Each attempt owns a heap context (event, `OVERLAPPED`, staging buffer) at a
//! fixed address. On deadline expiry the operation is cancelled with
//! `CancelIoEx` and drained for at most `CANCEL_DRAIN`. The context is freed
//! only after the kernel signals release; if it does not, the context is
//! leaked, because the kernel may still write into its buffer.

use std::io;
use std::os::windows::io::RawHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use super::{Transfer, CHUNK};

pub(super) type Raw = RawHandle;

const ERROR_IO_PENDING: i32 = 997;
const ERROR_OPERATION_ABORTED: i32 = 995;
const ERROR_BROKEN_PIPE: i32 = 109;
const ERROR_HANDLE_EOF: i32 = 38;

/// How long a cancelled operation may take to release its buffer.
const CANCEL_DRAIN: Duration = Duration::from_secs(1);

/// Handle validity is checked by the kernel on each call; nothing to verify
/// here. The overlapped requirement is documented on the public functions.
pub(super) fn check_mode(_raw: Raw) -> io::Result<()> {
    Ok(())
}

/// Heap context for one overlapped operation. Its address must stay fixed
/// while the kernel may reference it, so it is only ever held in a `Box`.
struct Op {
    event: HANDLE,
    overlapped: OVERLAPPED,
    buffer: Vec<u8>,
}

impl Drop for Op {
    fn drop(&mut self) {
        // SAFETY: `event` came from a successful CreateEventW and is closed once here.
        unsafe { CloseHandle(self.event) };
    }
}

enum Outcome {
    /// `n` bytes transferred. Zero on a read is end of stream.
    Done(usize),
    /// Cancelled before any byte transferred.
    Cancelled,
    /// The kernel did not release the context within `CANCEL_DRAIN`.
    Orphaned,
    /// The operation failed with an OS error.
    Failed(io::Error),
}

fn new_op(buffer: Vec<u8>) -> io::Result<Box<Op>> {
    // SAFETY: null security attributes and name give an unnamed, manual-reset,
    // non-signalled event.
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(Box::new(Op {
        event,
        overlapped: OVERLAPPED {
            hEvent: event,
            ..OVERLAPPED::default()
        },
        buffer,
    }))
}

/// Starts one overlapped transfer. Returns `true` if it completed at once.
fn issue(raw: Raw, op: &mut Op, write: bool) -> io::Result<bool> {
    // Buffers are at most CHUNK bytes, so the length fits in a u32.
    let len = op.buffer.len() as u32;
    let overlapped: *mut OVERLAPPED = &mut op.overlapped;
    // SAFETY: `buffer` and `overlapped` live in the boxed `op` at fixed
    // addresses. `run` does not return, and the box is not freed, until the
    // kernel releases them or the box is leaked on Orphaned.
    let ok = if write {
        unsafe {
            WriteFile(
                raw,
                op.buffer.as_ptr(),
                len,
                std::ptr::null_mut(),
                overlapped,
            )
        }
    } else {
        unsafe {
            ReadFile(
                raw,
                op.buffer.as_mut_ptr(),
                len,
                std::ptr::null_mut(),
                overlapped,
            )
        }
    };
    if ok != 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_IO_PENDING) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// Reads the status and count of a finished operation without waiting.
fn result(raw: Raw, op: &Op) -> io::Result<usize> {
    let mut transferred: u32 = 0;
    // SAFETY: the operation has completed, so `bWait = FALSE` only reads status.
    let ok = unsafe { GetOverlappedResult(raw, &op.overlapped, &mut transferred, 0) };
    if ok != 0 {
        Ok(transferred as usize)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A wait in milliseconds, rounded up and below `INFINITE`.
fn millis(wait: Duration) -> u32 {
    wait.as_nanos()
        .div_ceil(1_000_000)
        .min(u128::from(u32::MAX - 1)) as u32
}

/// Cancels a pending operation and waits for the kernel to release `op`.
fn cancel(raw: Raw, op: &Op) -> Outcome {
    // SAFETY: `op` was issued on `raw`. Cancelling a finished operation fails
    // harmlessly, and its status is still read below.
    unsafe { CancelIoEx(raw, &op.overlapped) };
    // SAFETY: the event is owned by `op`, which is alive for this call.
    if unsafe { WaitForSingleObject(op.event, millis(CANCEL_DRAIN)) } != WAIT_OBJECT_0 {
        return Outcome::Orphaned;
    }
    match result(raw, op) {
        Ok(n) => Outcome::Done(n),
        Err(err) if err.raw_os_error() == Some(ERROR_OPERATION_ABORTED) => Outcome::Cancelled,
        Err(err) => Outcome::Failed(err),
    }
}

/// Issues the operation and waits for it, returning at the deadline at the
/// latest. A completion that races the cancel still counts as a transfer.
fn run(raw: Raw, op: &mut Op, write: bool, deadline: Instant) -> Outcome {
    match issue(raw, op, write) {
        Ok(true) => {}
        Ok(false) => loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return cancel(raw, op);
            }
            // SAFETY: the event is owned by `op`, which is alive for this call.
            match unsafe { WaitForSingleObject(op.event, millis(left)) } {
                WAIT_OBJECT_0 => break,
                WAIT_TIMEOUT => continue,
                _ => {
                    let err = io::Error::last_os_error();
                    return match cancel(raw, op) {
                        Outcome::Orphaned => Outcome::Orphaned,
                        _ => Outcome::Failed(err),
                    };
                }
            }
        },
        Err(err) => return Outcome::Failed(err),
    }
    match result(raw, op) {
        Ok(n) => Outcome::Done(n),
        Err(err) => Outcome::Failed(err),
    }
}

fn orphaned() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "overlapped I/O was not released after cancellation; discard the handle",
    )
}

fn is_eof(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(ERROR_BROKEN_PIPE | ERROR_HANDLE_EOF)
    )
}

/// Reads at least one byte into `out`, or returns `Timeout` at the deadline.
/// `out` must be non-empty.
pub(super) fn read_some(raw: Raw, out: &mut [u8], deadline: Instant) -> io::Result<Transfer> {
    if deadline <= Instant::now() {
        return Ok(Transfer::Timeout);
    }
    let want = out.len().min(CHUNK);
    let mut op = new_op(vec![0u8; want])?;
    match run(raw, &mut op, false, deadline) {
        Outcome::Done(n) => {
            out[..n].copy_from_slice(&op.buffer[..n]);
            Ok(Transfer::Bytes(n))
        }
        Outcome::Cancelled => Ok(Transfer::Timeout),
        Outcome::Orphaned => {
            std::mem::forget(op);
            Err(orphaned())
        }
        Outcome::Failed(err) if is_eof(&err) => Ok(Transfer::Bytes(0)),
        Outcome::Failed(err) => Err(err),
    }
}

/// Writes at least one byte of `data`, or returns `Timeout` at the deadline.
/// `data` must be non-empty.
pub(super) fn write_some(raw: Raw, data: &[u8], deadline: Instant) -> io::Result<Transfer> {
    if deadline <= Instant::now() {
        return Ok(Transfer::Timeout);
    }
    let staged = data[..data.len().min(CHUNK)].to_vec();
    let mut op = new_op(staged)?;
    match run(raw, &mut op, true, deadline) {
        Outcome::Done(n) => Ok(Transfer::Bytes(n)),
        Outcome::Cancelled => Ok(Transfer::Timeout),
        Outcome::Orphaned => {
            std::mem::forget(op);
            Err(orphaned())
        }
        Outcome::Failed(err) => Err(err),
    }
}
