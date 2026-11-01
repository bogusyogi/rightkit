//! A native system prompt that collects a login's or a card's secret fields without the secret
//! ever passing through a web view: the caller opens the system's own credential dialog and moves
//! the typed values straight into its secure store. Nothing here logs, stores or returns the value
//! anywhere else, and the dialog's buffers are overwritten as soon as they are read.
//!
//! Windows: `CredUIPromptForCredentialsW` (credui.dll) with generic credentials, no certificates and
//! nothing persisted: two boxes, a visible one and a masked one. Raw declarations, so no `windows`
//! crate version is added. Other systems answer an error: apps there ask with a native secure field.

use zeroize::Zeroizing;

/// What the dialog collected: the visible box and the masked box (wiped on drop).
pub struct Entered {
    /// The visible box (for example a user name or a card holder).
    pub visible: String,
    /// The masked box, overwritten in memory when dropped.
    pub masked: Zeroizing<String>,
}

impl std::fmt::Debug for Entered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entered")
            .field("visible", &self.visible)
            .finish_non_exhaustive()
    }
}

/// Open the system credential dialog titled `caption` with `message`; `target` names what the
/// entry is for (shown by the system, never persisted). `Ok(None)` when the user cancels.
#[cfg(windows)]
pub fn ask(target: &str, caption: &str, message: &str) -> Result<Option<Entered>, String> {
    use std::ffi::c_void;
    use std::ptr::{null_mut, write_volatile};

    #[repr(C)]
    struct CredUiInfo {
        size: u32,
        parent: *mut c_void,
        message: *const u16,
        caption: *const u16,
        banner: *mut c_void,
    }

    #[link(name = "credui")]
    unsafe extern "system" {
        fn CredUIPromptForCredentialsW(
            info: *const CredUiInfo,
            target: *const u16,
            reserved: *mut c_void,
            auth_error: u32,
            user: *mut u16,
            user_len: u32,
            pass: *mut u16,
            pass_len: u32,
            save: *mut i32,
            flags: u32,
        ) -> u32;
    }

    // CREDUI_FLAGS_DO_NOT_PERSIST | EXCLUDE_CERTIFICATES | ALWAYS_SHOW_UI | GENERIC_CREDENTIALS
    const FLAGS: u32 = 0x2 | 0x8 | 0x80 | 0x40000;
    const ERROR_CANCELLED: u32 = 1223;
    // CREDUI_MAX_USERNAME_LENGTH + 1 and CREDUI_MAX_PASSWORD_LENGTH + 1, in UTF-16 units.
    const USER_UNITS: usize = 514;
    const PASS_UNITS: usize = 257;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn wipe(buffer: &mut [u16]) {
        for unit in buffer.iter_mut() {
            // SAFETY: a valid, exclusive element; volatile so the overwrite is not optimised away.
            unsafe { write_volatile(unit, 0) };
        }
    }
    fn read(buffer: &[u16]) -> String {
        let end = buffer.iter().position(|u| *u == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    let message = wide(message);
    let caption = wide(caption);
    let target = wide(target);
    let info = CredUiInfo {
        size: std::mem::size_of::<CredUiInfo>() as u32,
        parent: null_mut(),
        message: message.as_ptr(),
        caption: caption.as_ptr(),
        banner: null_mut(),
    };
    let mut user = vec![0u16; USER_UNITS];
    let mut pass = vec![0u16; PASS_UNITS];
    // SAFETY: every pointer is valid for the call (NUL-terminated strings, buffers of the sizes
    // passed); `save` is null because no save box is requested.
    let code = unsafe {
        CredUIPromptForCredentialsW(
            &info,
            target.as_ptr(),
            null_mut(),
            0,
            user.as_mut_ptr(),
            USER_UNITS as u32,
            pass.as_mut_ptr(),
            PASS_UNITS as u32,
            null_mut(),
            FLAGS,
        )
    };
    let out = match code {
        0 => Ok(Some(Entered {
            visible: read(&user),
            masked: Zeroizing::new(read(&pass)),
        })),
        ERROR_CANCELLED => Ok(None),
        other => Err(format!("The system prompt could not open (code {other}).")),
    };
    wipe(&mut user);
    wipe(&mut pass);
    out
}

/// Open the system credential dialog (unsupported on this system).
#[cfg(not(windows))]
pub fn ask(_target: &str, _caption: &str, _message: &str) -> Result<Option<Entered>, String> {
    Err("No native secret prompt on this system: ask with the platform's own secure field (for example a SwiftUI `SecureField`).".to_string())
}
