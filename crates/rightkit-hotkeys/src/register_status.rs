//! Typed classification of OS registration failures, shared by both platforms so the mapping is
//! one table that tests cover everywhere.

/// Why an OS hotkey registration was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterFailure {
    /// Another registration already owns the chord.
    Conflict,
    /// The chord or flags cannot be expressed as a system hotkey.
    Unsupported,
    /// The caller is not allowed to register (for example a different desktop or integrity level).
    PermissionDenied,
    /// The OS is out of memory or system resources.
    ResourceExhausted,
    /// Any other status; the caller keeps the raw code.
    Other,
}

impl RegisterFailure {
    /// Classify a Win32 `GetLastError` after a failed `RegisterHotKey`.
    pub fn from_win32(code: u32) -> Self {
        match code {
            1409 => Self::Conflict,                   // ERROR_HOTKEY_ALREADY_REGISTERED
            87 | 1004 => Self::Unsupported, // ERROR_INVALID_PARAMETER, ERROR_INVALID_FLAGS
            5 => Self::PermissionDenied,    // ERROR_ACCESS_DENIED
            8 | 14 | 1450 => Self::ResourceExhausted, // NOT_ENOUGH_MEMORY, OUTOFMEMORY, NO_SYSTEM_RESOURCES
            _ => Self::Other,
        }
    }

    /// Classify a Carbon `OSStatus` from `RegisterEventHotKey` (`0` is success and not a failure).
    pub fn from_osstatus(status: i32) -> Self {
        match status {
            -9878 => Self::Conflict,          // eventHotKeyExistsErr
            -9879 | -50 => Self::Unsupported, // eventHotKeyInvalidErr, paramErr
            -108 => Self::ResourceExhausted,  // memFullErr
            _ => Self::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win32_codes_map_to_typed_failures() {
        assert_eq!(RegisterFailure::from_win32(1409), RegisterFailure::Conflict);
        assert_eq!(
            RegisterFailure::from_win32(87),
            RegisterFailure::Unsupported
        );
        assert_eq!(
            RegisterFailure::from_win32(1004),
            RegisterFailure::Unsupported
        );
        assert_eq!(
            RegisterFailure::from_win32(5),
            RegisterFailure::PermissionDenied
        );
        for code in [8, 14, 1450] {
            assert_eq!(
                RegisterFailure::from_win32(code),
                RegisterFailure::ResourceExhausted
            );
        }
        assert_eq!(RegisterFailure::from_win32(1400), RegisterFailure::Other);
    }

    #[test]
    fn osstatus_codes_map_to_typed_failures() {
        assert_eq!(
            RegisterFailure::from_osstatus(-9878),
            RegisterFailure::Conflict
        );
        assert_eq!(
            RegisterFailure::from_osstatus(-9879),
            RegisterFailure::Unsupported
        );
        assert_eq!(
            RegisterFailure::from_osstatus(-50),
            RegisterFailure::Unsupported
        );
        assert_eq!(
            RegisterFailure::from_osstatus(-108),
            RegisterFailure::ResourceExhausted
        );
        assert_eq!(RegisterFailure::from_osstatus(-1), RegisterFailure::Other);
    }
}
