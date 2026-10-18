use std::{
    fs::File,
    io::{self, Read},
    os::fd::{AsRawFd, OwnedFd},
    process::{ChildStderr, ChildStdout},
};

pub(super) fn stdout(pipe: ChildStdout) -> File {
    File::from(OwnedFd::from(pipe))
}

pub(super) fn stderr(pipe: ChildStderr) -> File {
    File::from(OwnedFd::from(pipe))
}

pub(super) fn prepare(pipe: File) -> io::Result<File> {
    let fd = pipe.as_raw_fd();
    // ChildStdout/ChildStderr can also be built from arbitrary OwnedFd values. Reject
    // regular files/devices: O_NONBLOCK does not bound reads on those descriptor types.
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: live descriptor & writable storage for the native stat structure.
    if unsafe { libc::fstat(fd, metadata.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded & initialized the structure.
    if unsafe { metadata.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native capture requires a pipe",
        ));
    }
    // SAFETY: a borrowed, live pipe descriptor; F_GETFL takes no variadic argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_SETFL changes flags on this owned read end, preserving existing flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pipe)
}

pub(super) fn reader(pipe: File) -> File {
    pipe
}

pub(super) fn shutdown(_pipe: &mut File) -> io::Result<()> {
    Ok(())
}

pub(super) fn read(pipe: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    pipe.read(buffer)
}
