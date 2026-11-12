//! OS entropy for tokens and ids: the one place the shell reads it.

#[cfg(unix)]
pub(crate) fn fill_entropy(buf: &mut [u8]) -> std::io::Result<()> {
    extern "C" {
        fn getentropy(buf: *mut std::ffi::c_void, len: usize) -> std::ffi::c_int;
    }
    // SAFETY: `buf` is a valid writable slice of at most 256 bytes.
    if unsafe { getentropy(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(crate) fn fill_entropy(buf: &mut [u8]) -> std::io::Result<()> {
    #[link(name = "advapi32")]
    extern "system" {
        #[link_name = "SystemFunction036"]
        fn RtlGenRandom(buf: *mut u8, len: u32) -> u8;
    }
    // SAFETY: `buf` is a valid writable slice for `len` bytes.
    if unsafe { RtlGenRandom(buf.as_mut_ptr(), buf.len() as u32) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// `bytes` random bytes from the OS entropy source, as lowercase hex. At most 256 bytes.
#[cfg(any(feature = "viewer", feature = "update"))]
pub(crate) fn random_hex(bytes: usize) -> Result<String, crate::error::ShellError> {
    let mut buf = vec![0_u8; bytes.min(256)];
    fill_entropy(&mut buf).map_err(|e| {
        crate::error::ShellError::Other(format!("could not generate a random token: {e}"))
    })?;
    let mut out = String::with_capacity(buf.len() * 2);
    for byte in buf {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}
