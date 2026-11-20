//! Native machine-monotonic release samples, captured before dispatch.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeReleaseClock {
    WasapiQpc100nsV1,
    CoreAudioHostNsV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeReleaseEdge {
    pub clock: NativeReleaseClock,
    pub seconds: u64,
    pub nanos: u32,
}

#[cfg(any(windows, target_os = "macos"))]
fn from_nanos(clock: NativeReleaseClock, nanos: u128) -> Option<NativeReleaseEdge> {
    Some(NativeReleaseEdge {
        clock,
        seconds: u64::try_from(nanos / 1_000_000_000).ok()?,
        nanos: (nanos % 1_000_000_000) as u32,
    })
}

/// Samples only the native machine clock; no host callback runs here.
/// Failure is explicit: callers must not replace a missing edge with dispatch time.
#[cfg(windows)]
pub(crate) fn native_release_now() -> Option<NativeReleaseEdge> {
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    let mut frequency = 0i64;
    let mut counter = 0i64;
    // SAFETY: valid writable pointers; QPC retains neither pointer.
    let valid = unsafe {
        QueryPerformanceFrequency(&mut frequency) != 0
            && QueryPerformanceCounter(&mut counter) != 0
    };
    if !valid || frequency <= 0 || counter < 0 {
        return None;
    }
    let units_100ns = counter as u128 * 10_000_000 / frequency as u128;
    from_nanos(NativeReleaseClock::WasapiQpc100nsV1, units_100ns * 100)
}

#[cfg(target_os = "macos")]
pub(crate) fn native_release_now() -> Option<NativeReleaseEdge> {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    #[link(name = "System")]
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    let mut info = Timebase { numer: 0, denom: 0 };
    // SAFETY: the timebase pointer matches mach_timebase_info_data_t.
    let (ticks, status) = unsafe { (mach_absolute_time(), mach_timebase_info(&mut info)) };
    if status != 0 || info.numer == 0 || info.denom == 0 {
        return None;
    }
    from_nanos(
        NativeReleaseClock::CoreAudioHostNsV1,
        ticks as u128 * u128::from(info.numer) / u128::from(info.denom),
    )
}
