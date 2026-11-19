//! Read-only, content-free HID modifier observation. No event taps or prompts.

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::dictionary::CFDictionaryRef;
use core_foundation::string::CFString;
use std::ffi::{c_char, c_void};

const HID_SYSTEM_STATE: i32 = 1;
const SHIFT: u64 = 1 << 17;
const CONTROL: u64 = 1 << 18;
const OPTION: u64 = 1 << 19;
const COMMAND: u64 = 1 << 20;
const FUNCTION: u64 = 1 << 23;
const MODIFIER_FLAGS: u64 = SHIFT | CONTROL | OPTION | COMMAND | FUNCTION;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDictionaryGetValue(dictionary: CFDictionaryRef, key: *const c_void) -> *const c_void;
    fn CFGetTypeID(value: CFTypeRef) -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: *const c_void) -> u8;
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Hardware-event state for both sides of Shift, Control, Option & Command,
/// plus Fn. Caps Lock is a toggle & deliberately excluded.
///
/// This is the Quartz HID event-state table, not a per-device electrical
/// reading. Remapping, virtual hardware & HID-posted synthetic events may
/// influence it. Fields describe only the sampling interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalModifierState {
    pub left_shift: bool,
    pub right_shift: bool,
    pub left_control: bool,
    pub right_control: bool,
    pub left_option: bool,
    pub right_option: bool,
    pub left_command: bool,
    pub right_command: bool,
    pub function: bool,
}

impl PhysicalModifierState {
    /// True when any observed modifier is held. A successful false value is
    /// an instantaneous observation, never authorization for a later write.
    pub fn any_held(&self) -> bool {
        self.flags() != 0
    }

    fn flags(&self) -> u64 {
        (u64::from(self.left_shift || self.right_shift) * SHIFT)
            | (u64::from(self.left_control || self.right_control) * CONTROL)
            | (u64::from(self.left_option || self.right_option) * OPTION)
            | (u64::from(self.left_command || self.right_command) * COMMAND)
            | (u64::from(self.function) * FUNCTION)
    }
}

/// Missing evidence; none of these cases establishes that modifiers are up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierStateUnavailable {
    /// macOS has no nonprompting monitoring preflight (before macOS 10.15).
    MonitoringPreflightUnavailable,
    /// Input Monitoring preflight did not grant observation.
    InputMonitoringDenied,
    /// Secure Event Input can hide keyboard state.
    SecureInput,
    /// No readable, logged-in console session belongs to this process.
    ConsoleSessionUnavailable,
    /// Quartz could not create a HID state source.
    HidSourceUnavailable,
    /// Two reads or aggregate flags disagreed, e.g. during a key transition.
    InconsistentSample,
}

/// Sample physical modifiers without posting input, requesting permissions,
/// waiting, installing observers, or recording key content.
///
/// Requires macOS 10.15+ & a passing Input Monitoring preflight. Secure Input,
/// a missing/nonconsole GUI session, or inconsistent reads return `Err`;
/// callers must preserve unknown rather than treating it as all keys up.
/// Permission & session checks bracket two key sweeps & HID flag reads.
/// These OS calls are not atomic: a complete press/release between reads can
/// escape detection, & this API proves neither inactivity nor a quiet period.
/// Quartz provides no per-key error result; these checks reject known loss of
/// observability, not every possible device/driver failure.
pub fn physical_modifier_state() -> Result<PhysicalModifierState, ModifierStateUnavailable> {
    let preflight = monitoring_preflight()?;
    observable(preflight)?;
    let _source =
        super::keyboard::source().ok_or(ModifierStateUnavailable::HidSourceUnavailable)?;
    let flags_before = unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) } & MODIFIER_FLAGS;
    let first = read_modifiers(false);
    let second = read_modifiers(true);
    let flags_after = unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) } & MODIFIER_FLAGS;
    observable(preflight)?;
    if first != second || flags_before != flags_after || second.flags() != flags_after {
        return Err(ModifierStateUnavailable::InconsistentSample);
    }
    Ok(second)
}

type MonitoringPreflight = unsafe extern "C" fn() -> bool;

fn monitoring_preflight() -> Result<MonitoringPreflight, ModifierStateUnavailable> {
    // Dynamic lookup keeps older macOS hosts loadable & explicitly unknown.
    let pointer = unsafe {
        dlsym(
            -2isize as *mut c_void, // Darwin RTLD_DEFAULT
            c"CGPreflightListenEventAccess".as_ptr(),
        )
    };
    if pointer.is_null() {
        return Err(ModifierStateUnavailable::MonitoringPreflightUnavailable);
    }
    // Apple's CGEvent.h declares this exact no-argument C bool signature.
    Ok(unsafe { std::mem::transmute::<*mut c_void, MonitoringPreflight>(pointer) })
}

fn observable(preflight: MonitoringPreflight) -> Result<(), ModifierStateUnavailable> {
    if !unsafe { preflight() } {
        return Err(ModifierStateUnavailable::InputMonitoringDenied);
    }
    if unsafe { IsSecureEventInputEnabled() } != 0 {
        return Err(ModifierStateUnavailable::SecureInput);
    }
    let dictionary = unsafe { CGSessionCopyCurrentDictionary() };
    if dictionary.is_null() {
        return Err(ModifierStateUnavailable::ConsoleSessionUnavailable);
    }
    // Copy-rule ownership; release on every return path.
    let _owned = unsafe { CFType::wrap_under_create_rule(dictionary.cast()) };
    // CGSession.h expands kCGSessionOnConsoleKey to a string with two S characters.
    for name in ["kCGSSessionOnConsoleKey", "kCGSessionLoginDoneKey"] {
        let key = CFString::new(name);
        let value = unsafe { CFDictionaryGetValue(dictionary, key.as_CFTypeRef()) };
        if value.is_null()
            || unsafe { CFGetTypeID(value) != CFBooleanGetTypeID() }
            || unsafe { CFBooleanGetValue(value) } == 0
        {
            return Err(ModifierStateUnavailable::ConsoleSessionUnavailable);
        }
    }
    Ok(())
}

fn read_modifiers(reverse: bool) -> PhysicalModifierState {
    // HIToolbox Events.h virtual key codes; do not use aggregate flags to
    // infer either side. Reverse the second sweep to reduce transition skew.
    let keys = [0x38, 0x3c, 0x3b, 0x3e, 0x3a, 0x3d, 0x37, 0x36, 0x3f];
    let mut held = [false; 9];
    for index in 0..keys.len() {
        let slot = if reverse {
            keys.len() - 1 - index
        } else {
            index
        };
        held[slot] = unsafe { CGEventSourceKeyState(HID_SYSTEM_STATE, keys[slot]) };
    }
    PhysicalModifierState {
        left_shift: held[0],
        right_shift: held[1],
        left_control: held[2],
        right_control: held[3],
        left_option: held[4],
        right_option: held[5],
        left_command: held[6],
        right_command: held[7],
        function: held[8],
    }
}
