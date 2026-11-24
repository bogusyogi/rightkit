//! Feeding the mesh's async bulk send from a blocking reader (Pulse #846).
//!
//! [`Mesh::send_bulk_until`](crate::Mesh::send_bulk_until) reads its bytes asynchronously. A host with a
//! blocking `Read` (a file) makes a [`bulk_pipe`], runs [`pump_blocking`] on a thread of its own with the
//! writer end, and passes the reader end to the send. The pipe closes on every way out of the pump, so the
//! send sees the end of the data or a failed read instead of waiting forever.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncWriteExt, DuplexStream};
use tokio::runtime::Handle;

/// Bytes the pipe holds between the pump and the send.
const PIPE_BYTES: usize = 256 * 1024;
/// Bytes read from the file at a time.
const CHUNK_BYTES: usize = 64 * 1024;

/// The in-memory pipe: `(writer for the pump, reader for the send)`.
pub fn bulk_pipe() -> (DuplexStream, DuplexStream) {
    tokio::io::duplex(PIPE_BYTES)
}

/// Why the pump ended without delivering all `size` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PumpEnd {
    /// `cancel` was set.
    Cancelled,
    /// This computer could not read the data (or it ended early); the text is for the user.
    Local(String),
    /// The reader end closed: the send ended first.
    Pipe,
}

/// Copy `size` bytes from `data` into `writer`, calling `progress(total_so_far)` after each chunk and
/// checking `cancel` before each read. Blocks the calling thread: call it from a thread that is not part
/// of the runtime, with a [`Handle`] to the runtime that drives the send. `writer` is moved in and dropped on
/// every exit, which closes the pipe: on success the send sees the end after `size` bytes.
pub fn pump_blocking(
    data: &mut dyn Read,
    size: u64,
    mut writer: DuplexStream,
    runtime: &Handle,
    progress: &dyn Fn(u64),
    cancel: &AtomicBool,
) -> Result<(), PumpEnd> {
    let mut buffer = vec![0u8; CHUNK_BYTES];
    let mut done = 0u64;
    while done < size {
        if cancel.load(Ordering::Relaxed) {
            return Err(PumpEnd::Cancelled);
        }
        let want = usize::try_from((size - done).min(buffer.len() as u64)).unwrap_or(buffer.len());
        let n = data
            .read(&mut buffer[..want])
            .map_err(|e| PumpEnd::Local(format!("Couldn't read the file: {e}")))?;
        if n == 0 {
            return Err(PumpEnd::Local("The file ended early.".to_string()));
        }
        runtime
            .block_on(writer.write_all(&buffer[..n]))
            .map_err(|_| PumpEnd::Pipe)?;
        done += n as u64;
        progress(done);
    }
    Ok(())
}
