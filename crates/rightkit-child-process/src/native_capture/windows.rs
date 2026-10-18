use std::{
    cell::UnsafeCell,
    ffi::c_void,
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    process::{ChildStderr, ChildStdout},
};

const FILE_TYPE_PIPE: u32 = 3;
const PIPE_TYPE_MESSAGE: u32 = 4;
const FILE_MODE_INFORMATION: u32 = 16;
const FILE_SYNCHRONOUS_IO: u32 = 0x10 | 0x20;
const ERROR_BROKEN_PIPE: i32 = 109;
const ERROR_PIPE_NOT_CONNECTED: i32 = 233;
const ERROR_OPERATION_ABORTED: i32 = 995;
const ERROR_IO_INCOMPLETE: i32 = 996;
const ERROR_IO_PENDING: i32 = 997;
const ERROR_NOT_FOUND: i32 = 1168;

pub(super) fn stdout(pipe: ChildStdout) -> File {
    File::from(OwnedHandle::from(pipe))
}

pub(super) fn stderr(pipe: ChildStderr) -> File {
    File::from(OwnedHandle::from(pipe))
}

// Only owned handles cross the thread boundary. The pending request is allocated on
// the reader thread, so no raw pointer or in-flight request has to be declared Send.
pub(super) struct Prepared {
    pipe: File,
    event: OwnedHandle,
    disconnected: bool,
}

pub(super) fn prepare(pipe: File) -> io::Result<Prepared> {
    // ChildStdout may be constructed from any OwnedHandle, including a synchronous
    // pipe or regular file. OVERLAPPED alone cannot make a synchronous handle async.
    // Reject those handles before launching a reader rather than risk blocking ReadFile.
    // SAFETY: all queries borrow a live owned handle & use correctly sized output values.
    if unsafe { GetFileType(pipe.as_raw_handle()) } != FILE_TYPE_PIPE {
        return Err(unsupported());
    }
    let mut flags = 0;
    let mut disconnected = false;
    if unsafe {
        GetNamedPipeInfo(
            pipe.as_raw_handle(),
            &mut flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_PIPE_NOT_CONNECTED) {
            return Err(error);
        }
        // A disconnected handle is still a pipe (also recognized by .NET's
        // SafeFileHandle.GetPipeOrSocketType). It has no remaining readable stream.
        // Keep validating its asynchronous mode, then report EOF without issuing I/O.
        disconnected = true;
    }
    if flags & PIPE_TYPE_MESSAGE != 0 {
        return Err(unsupported());
    }
    let mut status = IoStatusBlock::default();
    let mut mode = 0u32;
    // Query the handle's actual mode, as SafeFileHandle.GetFileOptions does in .NET:
    // https://github.com/dotnet/runtime/blob/main/src/libraries/System.Private.CoreLib/src/Microsoft/Win32/SafeHandles/SafeFileHandle.Windows.cs
    // FileModeInformation reads file-object flags without a pending pipe read.
    let result = unsafe {
        NtQueryInformationFile(
            pipe.as_raw_handle(),
            &mut status,
            (&mut mode as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
            FILE_MODE_INFORMATION,
        )
    };
    if result != 0 {
        return Err(io::Error::other(format!(
            "cannot query native pipe mode (NTSTATUS {result:#x})"
        )));
    }
    if mode & FILE_SYNCHRONOUS_IO != 0 {
        return Err(unsupported());
    }
    // Manual-reset event: one request at a time, reset only after completion.
    // SAFETY: no inherited security attributes or name; booleans are Win32 BOOLs.
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(Prepared {
        pipe,
        // SAFETY: successful CreateEventW returned a new owned handle.
        event: unsafe { OwnedHandle::from_raw_handle(event) },
        disconnected,
    })
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "native capture requires a byte pipe opened for overlapped I/O",
    )
}

#[repr(C)]
#[derive(Default)]
struct IoStatusBlock {
    status_or_pointer: usize,
    information: usize,
}

// Win32 OVERLAPPED's offset union occupies two DWORDs on both 32- and 64-bit Windows.
#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: RawHandle,
}

impl Overlapped {
    fn new(event: RawHandle) -> Self {
        Self {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            // Suppress completion-port packets even if a caller associated this handle
            // with an IOCP before transferring it. Otherwise a queued completion could
            // expose this request's address after the capture has freed it.
            // GetOverlappedResult accepts this event-handle low-bit convention.
            event: ((event as usize) | 1) as RawHandle,
        }
    }
}

struct Request {
    // Windows may write these while read() is polling other Reader fields.
    overlapped: UnsafeCell<Overlapped>,
    buffer: UnsafeCell<[u8; super::CHUNK]>,
}

pub(super) struct Reader {
    prepared: Prepared,
    // Both addresses stay stable through ReadFile, cancellation & completion.
    request: Box<Request>,
    pending: bool,
}

pub(super) fn reader(prepared: Prepared) -> Reader {
    let request = Box::new(Request {
        overlapped: UnsafeCell::new(Overlapped::new(prepared.event.as_raw_handle())),
        buffer: UnsafeCell::new([0; super::CHUNK]),
    });
    Reader {
        prepared,
        request,
        pending: false,
    }
}

pub(super) fn read(pipe: &mut Reader, buffer: &mut [u8]) -> io::Result<usize> {
    if pipe.prepared.disconnected {
        return Ok(0);
    }
    if !pipe.pending {
        // SAFETY: owned event; the preceding request, if any, has completed.
        if unsafe { ResetEvent(pipe.prepared.event.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: no outstanding operation can access this storage before ReadFile.
        unsafe {
            *pipe.request.overlapped.get() = Overlapped::new(pipe.prepared.event.as_raw_handle());
        }
        // SAFETY: verified overlapped byte-pipe handle. Box owns the request & buffer
        // at stable addresses until GetOverlappedResult observes completion. An async
        // read never borrows the caller's buffer across this function's return.
        let ok = unsafe {
            ReadFile(
                pipe.prepared.pipe.as_raw_handle(),
                pipe.request.buffer.get().cast(),
                super::CHUNK as u32,
                std::ptr::null_mut(),
                pipe.request.overlapped.get(),
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(ERROR_IO_PENDING) => {}
                Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => return Ok(0),
                _ => return Err(error),
            }
        }
        pipe.pending = true;
    }
    let mut count = 0;
    // SAFETY: the exact live request passed to ReadFile; FALSE makes this a poll.
    if unsafe {
        GetOverlappedResult(
            pipe.prepared.pipe.as_raw_handle(),
            pipe.request.overlapped.get(),
            &mut count,
            0,
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        pipe.pending = false;
        return match error.raw_os_error() {
            Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(0),
            _ => Err(error),
        };
    }
    pipe.pending = false;
    let count = count as usize;
    // SAFETY: GetOverlappedResult observed completion, so Windows no longer writes
    // the buffer. No new operation is issued until after this copy returns.
    let bytes = unsafe { &*pipe.request.buffer.get() };
    buffer[..count].copy_from_slice(&bytes[..count]);
    Ok(count)
}

pub(super) fn shutdown(pipe: &mut Reader) -> io::Result<()> {
    if !pipe.pending {
        return Ok(());
    }
    // Cancellation only requests cancellation. Even ERROR_NOT_FOUND can mean the
    // request just completed; always observe completion before releasing its storage.
    // SAFETY: handle & exact request are still owned, with no concurrent reuse.
    let cancelled = unsafe {
        CancelIoEx(
            pipe.prepared.pipe.as_raw_handle(),
            pipe.request.overlapped.get(),
        )
    };
    let cancel_error = if cancelled == 0 {
        let error = io::Error::last_os_error();
        (error.raw_os_error() != Some(ERROR_NOT_FOUND)).then_some(error)
    } else {
        None
    };
    let mut count = 0;
    // SAFETY: wait for the native pipe request to complete or acknowledge cancellation.
    // Its buffer, OVERLAPPED, event & pipe remain owned throughout this wait. Unlike a
    // blocking read this wait is not extended by a descendant retaining a writer.
    let completed = unsafe {
        GetOverlappedResult(
            pipe.prepared.pipe.as_raw_handle(),
            pipe.request.overlapped.get(),
            &mut count,
            1,
        )
    };
    let completion_error = if completed == 0 {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(ERROR_OPERATION_ABORTED | ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => None,
            _ => Some(error),
        }
    } else {
        None
    };
    pipe.pending = false;
    cancel_error.or(completion_error).map_or(Ok(()), Err)
}

impl Drop for Reader {
    fn drop(&mut self) {
        // Also drain during unwinding: no path frees a request still owned by Windows.
        let _ = shutdown(self);
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFileType(handle: RawHandle) -> u32;
    fn GetNamedPipeInfo(
        handle: RawHandle,
        flags: *mut u32,
        out_buffer_size: *mut u32,
        in_buffer_size: *mut u32,
        max_instances: *mut u32,
    ) -> i32;
    fn CreateEventW(
        attributes: *const c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> RawHandle;
    fn ResetEvent(handle: RawHandle) -> i32;
    fn ReadFile(
        handle: RawHandle,
        buffer: *mut c_void,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn GetOverlappedResult(
        handle: RawHandle,
        overlapped: *mut Overlapped,
        bytes_transferred: *mut u32,
        wait: i32,
    ) -> i32;
    fn CancelIoEx(handle: RawHandle, overlapped: *mut Overlapped) -> i32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationFile(
        handle: RawHandle,
        status: *mut IoStatusBlock,
        information: *mut c_void,
        length: u32,
        information_class: u32,
    ) -> i32;
}
