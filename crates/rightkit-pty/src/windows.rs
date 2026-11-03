//! ConPTY: a pseudoconsole over two anonymous pipes, the child attached to it through
//! `OwnedCommand::windows_pseudoconsole`, and two bridge threads between the blocking pipes and
//! tokio. Written without a Windows host; see the changelog for the device run it needs.

use crate::PtySize;
use rightkit_child_process::{OwnedChild, OwnedCommand};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::windows::io::{FromRawHandle, OwnedHandle},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use windows::Win32::{
    Foundation::HANDLE,
    System::{
        Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole},
        Pipes::CreatePipe,
    },
};

/// Size of the in-memory buffer between a bridge thread and the async stream.
const BRIDGE_BUFFER: usize = 64 * 1024;

fn coord(size: PtySize) -> COORD {
    COORD {
        X: i16::try_from(size.cols).unwrap_or(i16::MAX),
        Y: i16::try_from(size.rows).unwrap_or(i16::MAX),
    }
}

/// An open pseudoconsole. Closing it ends the output pipe of the console once the child is gone;
/// `ClosePseudoConsole` can wait for the output to be read on some Windows builds, so it is
/// always called on its own short thread, never on the caller's.
struct Pseudo(HPCON);

// SAFETY: an HPCON is a handle value; the console API may be called from any thread.
unsafe impl Send for Pseudo {}

impl Drop for Pseudo {
    fn drop(&mut self) {
        let raw = self.0.0;
        let _ = std::thread::Builder::new()
            .name("rightkit-pty-close".into())
            .spawn(move || {
                // SAFETY: the handle came from CreatePseudoConsole and is closed exactly once,
                // here, after nothing can use it any more.
                unsafe { ClosePseudoConsole(HPCON(raw)) };
            });
    }
}

#[derive(Debug)]
pub(crate) struct Terminal {
    pseudo: Arc<Mutex<Option<Pseudo>>>,
    // The two ends this process keeps: bytes written here reach the child, bytes the child
    // prints are read from here. Taken by `reader` / `writer`.
    input: Mutex<Option<File>>,
    output: Mutex<Option<File>>,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for Pseudo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pseudo")
    }
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    // SAFETY: valid out pointers; default security (not inheritable), default buffer size.
    unsafe { CreatePipe(&mut read, &mut write, None, 0) }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xFFFF))?;
    // SAFETY: CreatePipe returned two new handles that nothing else owns.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read.0 as _),
            OwnedHandle::from_raw_handle(write.0 as _),
        )
    })
}

pub(crate) fn spawn(
    mut command: OwnedCommand,
    size: PtySize,
) -> io::Result<(OwnedChild, Terminal)> {
    let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
    // The console reads `input_read` (what we write) and writes `output_write` (what we read).
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;
    // SAFETY: both handles are open; CreatePseudoConsole duplicates what it needs.
    let hpcon = unsafe {
        use std::os::windows::io::AsRawHandle;
        CreatePseudoConsole(
            coord(size),
            HANDLE(input_read.as_raw_handle() as _),
            HANDLE(output_write.as_raw_handle() as _),
            0,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xFFFF))?;
    let pseudo = Pseudo(hpcon);
    // The console owns its copies now; ours must close or the pipes never end.
    drop(input_read);
    drop(output_write);
    // SAFETY: `hpcon` is a live pseudoconsole that `pseudo` keeps open past the spawn call.
    unsafe { command.windows_pseudoconsole(hpcon.0) };
    // On failure `pseudo` drops (closing the console) and the pipe ends close with it.
    let child = command.spawn()?;
    Ok((
        child,
        Terminal {
            pseudo: Arc::new(Mutex::new(Some(pseudo))),
            input: Mutex::new(Some(File::from(input_write))),
            output: Mutex::new(Some(File::from(output_read))),
            runtime,
        },
    ))
}

impl Terminal {
    pub(crate) fn reader(&self) -> io::Result<RawReader> {
        let pipe = self
            .output
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .ok_or_else(|| io::Error::other("the terminal output was already taken"))?;
        bridge_reader(pipe, &self.runtime)
    }

    pub(crate) fn writer(&self) -> io::Result<RawWriter> {
        let pipe = self
            .input
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .ok_or_else(|| io::Error::other("the terminal input was already taken"))?;
        bridge_writer(pipe, &self.runtime)
    }

    /// Ctrl-Z then Enter: the console's end-of-input line.
    pub(crate) fn eof_sequence(&self) -> Vec<u8> {
        vec![0x1a, b'\r']
    }

    pub(crate) fn resize(&self, size: PtySize) -> io::Result<()> {
        let guard = self
            .pseudo
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(pseudo) = guard.as_ref() else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the pseudoconsole is closed",
            ));
        };
        // SAFETY: the handle is open while it is in the slot (closed only by taking it out).
        unsafe { ResizePseudoConsole(pseudo.0, coord(size)) }
            .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xFFFF))
    }

    /// The child exited: close the pseudoconsole so its output pipe ends after the last byte.
    pub(crate) fn child_exited(&self) {
        let taken = self
            .pseudo
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        drop(taken);
    }
}

pub(crate) type RawReader = BridgeReader;
pub(crate) type RawWriter = BridgeWriter;

/// The async read end of a thread that pumps the output pipe.
#[derive(Debug)]
pub(crate) struct BridgeReader(DuplexStream);

/// The async write end of a thread that pumps the input pipe.
#[derive(Debug)]
pub(crate) struct BridgeWriter(DuplexStream);

fn bridge_reader(mut pipe: File, runtime: &tokio::runtime::Handle) -> io::Result<BridgeReader> {
    let (mine, mut theirs) = tokio::io::duplex(BRIDGE_BUFFER);
    let runtime = runtime.clone();
    std::thread::Builder::new()
        .name("rightkit-pty-read".into())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        // The reader dropped its end: stop, which closes the pipe.
                        if runtime
                            .block_on(theirs.write_all(&buffer[..count]))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    // ERROR_BROKEN_PIPE: the console closed its end.
                    Err(_) => break,
                }
            }
            // End of file for the async reader.
            let _ = runtime.block_on(theirs.shutdown());
        })?;
    Ok(BridgeReader(mine))
}

fn bridge_writer(mut pipe: File, runtime: &tokio::runtime::Handle) -> io::Result<BridgeWriter> {
    let (mine, mut theirs) = tokio::io::duplex(BRIDGE_BUFFER);
    let runtime = runtime.clone();
    std::thread::Builder::new()
        .name("rightkit-pty-write".into())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match runtime.block_on(theirs.read(&mut buffer)) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if pipe
                            .write_all(&buffer[..count])
                            .and_then(|()| pipe.flush())
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            // Dropping `pipe` here closes the console's input.
        })?;
    Ok(BridgeWriter(mine))
}

impl AsyncRead for BridgeReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for BridgeWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
