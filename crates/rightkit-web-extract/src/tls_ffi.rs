//! The native tls-client (Go c-shared) FFI boundary: library cache, bounded response reads and
//! the RAII response guard.

use std::collections::HashMap;
use std::ffi::{c_char, CString};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::tls::TlsClientResponse;

fn tls_library_cache() -> &'static Mutex<HashMap<std::path::PathBuf, Arc<libloading::Library>>> {
    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, Arc<libloading::Library>>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn load_tls_library(
    path: &Path,
    trusted: &dyn Fn(&Path) -> bool,
) -> Result<Arc<libloading::Library>, String> {
    let cache = tls_library_cache();
    let mut cache = cache
        .lock()
        .map_err(|_| "tls-client library cache poisoned".to_string())?;
    if let Some(lib) = cache.get(path) {
        return Ok(Arc::clone(lib));
    }
    if !trusted(path) {
        return Err(format!(
            "refusing to load {}: outside the installed bundle",
            path.display()
        ));
    }
    // Keep each loaded Go c-shared library resident for process lifetime. Its
    // runtime owns background threads that may outlive one request; unloading
    // per call lets those threads execute unmapped code and crash the sidecar.
    let lib = unsafe { libloading::Library::new(path) }
        .map_err(|e| format!("load {}: {e}", path.display()))?;
    let lib = Arc::new(lib);
    cache.insert(path.to_path_buf(), Arc::clone(&lib));
    Ok(lib)
}

pub const MAX_TLS_CLIENT_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Most raw third-party text an error message may quote (audit B6: a 16 MiB response must
/// not reach errors, the queue file, or logs).
pub const MAX_ERROR_PREVIEW_BYTES: usize = 2048;

/// The first `max_bytes` of `text`, cut back to a UTF-8 character boundary.
pub fn bounded_preview(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Read up to `max_bytes` of a NUL-terminated C string from an FFI pointer
/// without trusting the producer to actually place a NUL within `max_bytes`.
///
/// Returns `Err` if the pointer is null, the string is empty, or the producer
/// failed to NUL-terminate within `max_bytes`.
///
/// # Safety
///
/// `ptr` must point to memory the caller has read access to. The
/// caller is responsible for freeing the underlying buffer (e.g. via the
/// tls-client `freeMemory` function once we have parsed out the response id).
pub unsafe fn read_bounded_c_string(
    ptr: *const c_char,
    max_bytes: usize,
) -> Result<String, String> {
    if ptr.is_null() {
        return Err("tls-client returned a null response pointer".to_string());
    }
    if max_bytes == 0 {
        return Err("tls-client response cap is zero".to_string());
    }
    let mut len = 0usize;
    while len < max_bytes {
        // Safety: bounds-checked read; we never cross `max_bytes`.
        let byte = unsafe { *ptr.add(len) } as u8;
        if byte == 0 {
            if len == 0 {
                return Err("tls-client returned an empty response".to_string());
            }
            let mut bytes = vec![0u8; len];
            // Safety: src+dst valid for `len` bytes, no overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(ptr, bytes.as_mut_ptr() as *mut c_char, len);
            }
            return String::from_utf8(bytes)
                .map_err(|_| "tls-client returned non-UTF8 response".to_string());
        }
        len += 1;
    }
    // Reached `max_bytes` without seeing a NUL — refuse the response.
    Err(format!(
        "tls-client response exceeded {max_bytes} bytes without a NUL terminator"
    ))
}

/// RAII handle for a single tls-client invocation.
///
/// Keeps a reference to the process-lifetime library cache & calls
/// `freeMemory(response.id)` so a
/// successfully-parsed response is released from the DLL's Go-side
/// `unsafePointers` map. If the response is malformed and we never learn
/// its id, Drop simply skips the free — the DLL frees those on process
/// shutdown.
pub struct TlsClientSessionGuard {
    _lib: Arc<libloading::Library>,
    free_memory: unsafe extern "C" fn(*const c_char),
    response_id: Option<CString>,
}

impl TlsClientSessionGuard {
    pub fn new(
        lib: Arc<libloading::Library>,
        free_memory: unsafe extern "C" fn(*const c_char),
    ) -> Self {
        Self {
            _lib: lib,
            free_memory,
            response_id: None,
        }
    }

    pub fn set_response_id(&mut self, id: &str) {
        // `response.id` is a UUID-shaped ASCII string; if it ever contains an
        // interior NUL we just refuse to track it and rely on the DLL's
        // session-cleanup path to free it.
        self.response_id = CString::new(id).ok();
    }
}

impl Drop for TlsClientSessionGuard {
    fn drop(&mut self) {
        if let Some(id) = self.response_id.as_ref() {
            // Best-effort: never propagate errors from a Drop impl.
            unsafe { (self.free_memory)(id.as_ptr()) };
        }
    }
}

/// Public alias matching the tls-client `freeMemory` C signature.
pub type FreeFn = unsafe extern "C" fn(*const c_char);

/// Thin wrapper around `libloading::Library::new` + symbol lookup + JSON
/// parse, holding the result id in the RAII guard so the DLL releases it on
/// Drop. The body of the FFI call lives here (not in `article_tls.rs`) so
/// that the FFI helpers above stay together with the unsafe boundary they
/// defend.
///
/// # Safety
///
/// The loaded library is third-party code; pointer returns are untrusted and bounded via
/// `read_bounded_c_string`. `dll` must be a tls-client shared library.
pub unsafe fn call_tls_client_request(
    dll: &Path,
    payload: &str,
    trusted: &dyn Fn(&Path) -> bool,
) -> Result<TlsClientResponse, String> {
    type RequestFn = unsafe extern "C" fn(*const c_char) -> *mut c_char;

    // Trust boundary: the DLL on disk is third-party code loaded into our
    // process. Treat every pointer it returns as untrusted: bound the read,
    // parse defensively, and retain its Go runtime for process lifetime. The
    // response buffer is owned by the DLL and freed via `freeMemory(id)`.
    let lib = load_tls_library(dll, trusted)?;
    let request: RequestFn = *unsafe { lib.get::<RequestFn>(b"request\0") }
        .map_err(|e| format!("load request symbol: {e}"))?;
    let free_memory: FreeFn = *unsafe { lib.get::<FreeFn>(b"freeMemory\0") }
        .map_err(|e| format!("load freeMemory symbol: {e}"))?;
    let mut guard = TlsClientSessionGuard::new(lib, free_memory);
    let c_payload = CString::new(payload).map_err(|_| "tls-client payload contains NUL")?;
    let ptr = unsafe { request(c_payload.as_ptr()) };
    if ptr.is_null() {
        return Err("tls-client returned a null response pointer".to_string());
    }
    let response_json = unsafe { read_bounded_c_string(ptr, MAX_TLS_CLIENT_RESPONSE_BYTES) }?;
    let response: TlsClientResponse = serde_json::from_str(&response_json).map_err(|e| {
        format!(
            "parse tls-client response JSON: {e}; raw={}",
            bounded_preview(&response_json, MAX_ERROR_PREVIEW_BYTES)
        )
    })?;
    // If parse fails mid-way, Drop runs without an id and skips the free;
    // the DLL releases unacknowledged responses at session destruction.
    guard.set_response_id(&response.id);
    Ok(response)
}

#[cfg(test)]
mod preview_tests {
    use super::bounded_preview;

    #[test]
    fn preview_returns_short_text_unchanged() {
        assert_eq!(bounded_preview("{\"id\":1}", 2048), "{\"id\":1}");
    }

    #[test]
    fn preview_caps_long_text_at_the_limit() {
        let raw = "x".repeat(16 * 1024);
        assert_eq!(bounded_preview(&raw, 2048).len(), 2048);
    }

    #[test]
    fn preview_never_splits_a_multibyte_character() {
        // 'é' is two bytes; a cut at an odd byte index must back off to the boundary.
        let raw = "é".repeat(100);
        let preview = bounded_preview(&raw, 9);
        assert_eq!(preview, "éééé");
    }
}
