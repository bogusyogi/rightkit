//! Low-level Accessibility (AX) FFI bindings and primitive AX attribute
//! readers shared across the macos submodules. Pure move from the former
//! monolithic `macos.rs`; no behaviour change.

use core_foundation::base::{CFType, TCFType};
use core_foundation::string::{CFString, CFStringRef};

use crate::macos::ax::{system_wide_with, AxOptions, El};

use super::observation::observation_clients;

pub(super) type AXUIElementRef = *const std::ffi::c_void;

pub(super) const PROC_PIDTBSDINFO: i32 = 3;

pub(super) const PROC_BSD_INFO_SIZE: i32 = 136;

/// Darwin's `struct proc_bsdinfo` prefix needed for a process start identity.
/// Keep this local so platform identity does not depend on a second crate's
/// libc layout. The SDK struct is 136 bytes on supported macOS targets.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pub(super) pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [std::ffi::c_char; 16],
    pbi_name: [std::ffi::c_char; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pub(super) pbi_start_tvsec: u64,
    pub(super) pbi_start_tvusec: u64,
}

const _: [(); PROC_BSD_INFO_SIZE as usize] = [(); std::mem::size_of::<ProcBsdInfo>()];

#[allow(non_upper_case_globals)]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub(super) fn AXIsProcessTrusted() -> u8;
    pub(super) fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    pub(super) static kAXTrustedCheckOptionPrompt: CFStringRef;
    pub(super) fn AXUIElementCopyParameterizedAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        parameter: *const std::ffi::c_void,
        value: *mut *const std::ffi::c_void,
    ) -> i32;
    pub(super) fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> i32;
    pub(super) fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> i32;
    pub(super) fn AXUIElementSetMessagingTimeout(
        element: AXUIElementRef,
        timeout_in_seconds: f32,
    ) -> i32;
    pub(super) fn AXValueGetValue(
        value: *const std::ffi::c_void,
        the_type: u32,
        value_ptr: *mut std::ffi::c_void,
    ) -> u8;
}

#[link(name = "proc")]
extern "C" {
    pub(super) fn proc_pidinfo(
        pid: i32,
        flavor: i32,
        arg: u64,
        buffer: *mut std::ffi::c_void,
        buffersize: i32,
    ) -> i32;
}

/// kAXValueCFRangeType — the AXValue tag wrapping a CFRange.
pub(super) const K_AX_VALUE_CFRANGE_TYPE: u32 = 4;

pub(super) const K_AX_VALUE_CGRECT_TYPE: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct AxCFRange {
    location: isize,
    length: isize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct AxCGPoint {
    pub(super) x: f64,
    pub(super) y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct AxCGSize {
    pub(super) width: f64,
    pub(super) height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct AxCGRect {
    pub(super) origin: AxCGPoint,
    pub(super) size: AxCGSize,
}

pub(super) const AX_MESSAGING_TIMEOUT_SECONDS: f32 = 0.1;

fn ax_options() -> AxOptions {
    AxOptions::with_timeout(std::time::Duration::from_secs_f32(
        AX_MESSAGING_TIMEOUT_SECONDS,
    ))
}

/// Application element for `pid` from the RightKit AX owner, already at the
/// finite the app messaging timeout. Owned as a retained `CFType`.
pub(super) fn ax_app_element(pid: i32) -> Option<CFType> {
    let element = El::app_with(pid, &ax_options()).ok()?;
    Some(unsafe { CFType::wrap_under_get_rule(element.as_raw()) })
}

/// System-wide element from the RightKit AX owner, at the finite the app
/// messaging timeout. Owned as a retained `CFType`.
pub(super) fn ax_system_element() -> Option<CFType> {
    let element = system_wide_with(&ax_options()).ok()?;
    Some(unsafe { CFType::wrap_under_get_rule(element.as_raw()) })
}

/// Boolean AX write through the RightKit owner. `true` only when the AX setter
/// reported success.
pub(super) unsafe fn ax_set_bool(element: AXUIElementRef, attr: &str, value: bool) -> bool {
    El::retain_raw(element).is_some_and(|el| el.set_bool(attr, value).is_ok())
}

/// String AX write through the RightKit owner. `true` only when the AX setter
/// reported success.
pub(super) unsafe fn ax_set_str(element: AXUIElementRef, attr: &str, value: &str) -> bool {
    El::retain_raw(element).is_some_and(|el| el.set_str(attr, value).is_ok())
}
// Before and after snapshots share this 64 KiB transaction budget.

pub(super) const MAX_PROOF_UTF16_UNITS: isize = 32_768;

pub(super) const MAX_PROOF_UTF8_BYTES: usize = 32_768;

pub(super) unsafe fn ax_copy(
    element: AXUIElementRef,
    attr: &'static str,
) -> Option<*const std::ffi::c_void> {
    ax_copy_with_error(element, attr).ok()
}

pub(super) unsafe fn ax_copy_with_error(
    element: AXUIElementRef,
    attr: &'static str,
) -> Result<*const std::ffi::c_void, i32> {
    // RightKit owns the raw AX copy; hand the +1 retained object to the caller
    // (who wraps it under the create rule) by forgetting the owning guard.
    let el = El::retain_raw(element).ok_or(-25202)?;
    let cf = el.copy_attribute_raw(attr)?;
    let ptr = cf.0;
    std::mem::forget(cf);
    Ok(ptr)
}

pub(super) fn ax_error_label(error: i32) -> &'static str {
    match error {
        -25212 => "no_value",
        -25202 => "invalid_ui_element",
        -25204 => "cannot_complete",
        -25205 => "attribute_unsupported",
        -25208 => "not_implemented",
        _ => "other",
    }
}

pub(super) fn log_ax_query_failure(
    scope: &str,
    attribute: &str,
    error: i32,
    started: std::time::Instant,
) {
    eprintln!(
        "ax_query_failed scope={scope} attribute={attribute} ax_error={error} class={} duration_ms={} observation_clients={}",
        ax_error_label(error),
        started.elapsed().as_millis(),
        observation_clients().lock().map(|clients| clients.values().sum::<usize>()).unwrap_or(0),
    );
}

pub(super) unsafe fn ax_element_pid(element: AXUIElementRef) -> Option<i32> {
    let mut pid = 0i32;
    (AXUIElementGetPid(element, &mut pid) == 0 && pid > 0).then_some(pid)
}

pub(super) unsafe fn ax_string(element: AXUIElementRef, attr: &'static str) -> Option<String> {
    let value = ax_copy(element, attr)?;
    core_foundation::base::CFType::wrap_under_create_rule(value)
        .downcast_into::<CFString>()
        .map(|s| s.to_string())
}

pub(super) unsafe fn ax_string_bounded(
    element: AXUIElementRef,
    attr: &'static str,
) -> Option<String> {
    let value = ax_copy(element, attr)?;
    let value =
        core_foundation::base::CFType::wrap_under_create_rule(value).downcast_into::<CFString>()?;
    bounded_proof_string(value)
}

pub(super) fn bounded_proof_string(value: CFString) -> Option<String> {
    // Check CFString's UTF-16 length before conversion. A byte check after
    // conversion covers non-ASCII text without retaining oversized proof.
    if value.char_len() > MAX_PROOF_UTF16_UNITS {
        return None;
    }
    let text = value.to_string();
    (text.len() <= MAX_PROOF_UTF8_BYTES).then_some(text)
}

pub(super) unsafe fn ax_bool(element: AXUIElementRef, attr: &'static str) -> Option<bool> {
    let value = ax_copy(element, attr)?;
    CFType::wrap_under_create_rule(value)
        .downcast_into::<CFBoolean>()
        .map(bool::from)
}

pub(super) unsafe fn ax_attribute_settable(element: AXUIElementRef, attr: &'static str) -> bool {
    let attr = CFString::from_static_string(attr);
    let mut settable = 0u8;
    AXUIElementIsAttributeSettable(element, attr.as_concrete_TypeRef(), &mut settable) == 0
        && settable != 0
}

pub(super) unsafe fn ax_parameterized_copy(
    element: AXUIElementRef,
    attr: &'static str,
    parameter: *const std::ffi::c_void,
) -> Option<*const std::ffi::c_void> {
    let attr = CFString::from_static_string(attr);
    let mut out: *const std::ffi::c_void = std::ptr::null();
    let err = AXUIElementCopyParameterizedAttributeValue(
        element,
        attr.as_concrete_TypeRef(),
        parameter,
        &mut out,
    );
    (err == 0 && !out.is_null()).then_some(out)
}

pub(super) unsafe fn ax_children(element: AXUIElementRef) -> Vec<CFType> {
    ax_copy(element, "AXChildren")
        .map(|children| {
            let array: core_foundation::array::CFArray<CFType> =
                core_foundation::array::CFArray::wrap_under_create_rule(children as _);
            array.iter().map(|child| child.clone()).collect()
        })
        .unwrap_or_default()
}

pub(super) unsafe fn ax_dom_classes(element: AXUIElementRef) -> Vec<String> {
    ax_copy(element, "AXDOMClassList")
        .map(|classes| {
            let array: core_foundation::array::CFArray<CFType> =
                core_foundation::array::CFArray::wrap_under_create_rule(classes as _);
            array
                .iter()
                .filter_map(|class| class.clone().downcast_into::<CFString>())
                .map(|class| class.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// UTF-16 selection range of the focused element, the unit AX itself uses.
pub(super) unsafe fn ax_selected_range(element: AXUIElementRef) -> Option<(usize, usize)> {
    let raw = ax_copy(element, "AXSelectedTextRange")?;
    let owned = CFType::wrap_under_create_rule(raw);
    let mut range = AxCFRange::default();
    let ok = AXValueGetValue(
        owned.as_CFTypeRef(),
        K_AX_VALUE_CFRANGE_TYPE,
        &mut range as *mut AxCFRange as *mut std::ffi::c_void,
    );
    if ok == 0 || range.location < 0 || range.length < 0 {
        return None;
    }
    Some((range.location as usize, range.length as usize))
}

pub(super) unsafe fn ax_frame(element: AXUIElementRef) -> Option<AxCGRect> {
    let raw = ax_copy(element, "AXFrame")?;
    let owned = CFType::wrap_under_create_rule(raw);
    let mut frame = AxCGRect::default();
    (AXValueGetValue(
        owned.as_CFTypeRef(),
        K_AX_VALUE_CGRECT_TYPE,
        &mut frame as *mut AxCGRect as *mut std::ffi::c_void,
    ) != 0)
        .then_some(frame)
}
