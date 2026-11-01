//! GPU temperature: the NVIDIA GPU's temperature, one line every few seconds, for the
//! Windows notch and hub. NVIDIA's library (`nvml.dll`) takes about 20 MB of the process that
//! loads it and Windows does not give that back on unload, so the notch never loads it:
//! it runs this command only while someone looks at the System readings and ends it after.
//!
//! Output: one line per reading: the hottest GPU's core temperature in whole degrees Celsius
//! (or `none` when no GPU answered that time), a space, then the fans as comma-separated
//! tokens, `r1450` for revolutions per minute (`nvmlDeviceGetFanSpeedRPM`, newer drivers) or
//! `p42` for percent of maximum (`nvmlDeviceGetFanSpeed_v2` / `nvmlDeviceGetFanSpeed`), or
//! `-` when the driver reports no fan. A fan that does not answer is left out, never 0. It ends when its standard input closes (the
//! parent went away or lost interest). Exit code 2, with no output, when there is no NVIDIA
//! driver or GPU.

/// Prints readings until standard input closes; returns the process's exit code.
#[cfg(not(windows))]
pub fn watch(_every: std::time::Duration) -> i32 {
    2
}

#[cfg(windows)]
pub use imp::watch;

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io::{Read, Write};
    use std::time::Duration;

    #[allow(non_snake_case)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    /// The system directory only: a copy next to the program is never loaded.
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
    /// GPUs asked, at most.
    const MAX_DEVICES: u32 = 8;

    type Device = *mut c_void;
    type GetTemperature = unsafe extern "C" fn(Device, i32, *mut u32) -> i32;
    /// `nvmlDeviceGetNumFans(device, &mut count)`.
    type GetNumFans = unsafe extern "C" fn(Device, *mut u32) -> i32;
    /// `nvmlDeviceGetFanSpeed_v2(device, fan, &mut percent)`.
    type GetFanSpeedV2 = unsafe extern "C" fn(Device, u32, *mut u32) -> i32;
    /// `nvmlDeviceGetFanSpeed(device, &mut percent)`: the first fan only.
    type GetFanSpeed = unsafe extern "C" fn(Device, *mut u32) -> i32;
    /// `nvmlDeviceGetFanSpeedRPM(device, &mut nvmlFanSpeedInfo_t)`.
    type GetFanSpeedRpm = unsafe extern "C" fn(Device, *mut FanSpeedInfo) -> i32;

    /// `nvmlFanSpeedInfo_t`: `version` is `NVML_STRUCT_VERSION(FanSpeedInfo, 1)`, the struct's
    /// size with 1 in the top byte; `fan` is the index asked; `speed` comes back in rpm.
    #[repr(C)]
    struct FanSpeedInfo {
        version: u32,
        fan: u32,
        speed: u32,
    }
    const FAN_SPEED_INFO_VERSION: u32 = (std::mem::size_of::<FanSpeedInfo>() as u32) | (1 << 24);
    /// Fans asked per GPU, at most.
    const MAX_FANS: u32 = 8;

    /// One fan's reading, as the driver gave it.
    enum Fan {
        Rpm(u32),
        Percent(u32),
    }

    /// One look: the hottest temperature and every fan that answered.
    struct Sample {
        celsius: Option<u32>,
        fans: Vec<Fan>,
    }

    struct Nvml {
        devices: Vec<Device>,
        /// `nvmlDeviceGetTemperature(device, NVML_TEMPERATURE_GPU = 0, &mut degrees_celsius)`.
        get_temperature: GetTemperature,
        /// Each of these is absent on drivers that predate it; none is required.
        num_fans: Option<GetNumFans>,
        fan_rpm: Option<GetFanSpeedRpm>,
        fan_percent_v2: Option<GetFanSpeedV2>,
        fan_percent: Option<GetFanSpeed>,
    }

    fn export(module: *mut c_void, name: &'static [u8]) -> Option<*mut c_void> {
        // SAFETY: `module` is a loaded library and `name` is NUL-terminated.
        let found = unsafe { GetProcAddress(module, name.as_ptr()) };
        (!found.is_null()).then_some(found)
    }

    impl Nvml {
        /// Loads the library and the first few GPUs; `None` without an NVIDIA driver or GPU.
        fn open() -> Option<Self> {
            let name: Vec<u16> = "nvml.dll".encode_utf16().chain([0]).collect();
            // SAFETY: a NUL-terminated name; the library stays loaded for this short process.
            let module = unsafe {
                LoadLibraryExW(
                    name.as_ptr(),
                    std::ptr::null_mut(),
                    LOAD_LIBRARY_SEARCH_SYSTEM32,
                )
            };
            if module.is_null() {
                return None;
            }
            // SAFETY (the four transmutes): the exports have exactly these C signatures in
            // NVML's public header, and a data pointer and a function pointer are one size.
            let init: unsafe extern "C" fn() -> i32 =
                unsafe { std::mem::transmute(export(module, b"nvmlInit_v2\0")?) };
            let count: unsafe extern "C" fn(*mut u32) -> i32 =
                unsafe { std::mem::transmute(export(module, b"nvmlDeviceGetCount_v2\0")?) };
            let device_at: unsafe extern "C" fn(u32, *mut Device) -> i32 =
                unsafe { std::mem::transmute(export(module, b"nvmlDeviceGetHandleByIndex_v2\0")?) };
            let get_temperature: GetTemperature =
                unsafe { std::mem::transmute(export(module, b"nvmlDeviceGetTemperature\0")?) };
            // SAFETY: `init` takes no arguments; NVML_SUCCESS is 0.
            if unsafe { init() } != 0 {
                return None;
            }
            let mut found = 0u32;
            // SAFETY: `found` is a valid out pointer for the call.
            if unsafe { count(&mut found) } != 0 {
                return None;
            }
            let mut devices = Vec::new();
            for index in 0..found.min(MAX_DEVICES) {
                let mut handle: Device = std::ptr::null_mut();
                // SAFETY: `handle` is a valid out pointer; the handle stays valid while NVML
                // is initialised, which is for the rest of this process.
                if unsafe { device_at(index, &mut handle) } == 0 && !handle.is_null() {
                    devices.push(handle);
                }
            }
            // SAFETY (the four transmutes): the NVML header's signatures; an absent export
            // (an older driver) stays `None` and the next fallback is used.
            let num_fans = export(module, b"nvmlDeviceGetNumFans\0")
                .map(|f| unsafe { std::mem::transmute::<*mut c_void, GetNumFans>(f) });
            let fan_rpm = export(module, b"nvmlDeviceGetFanSpeedRPM\0")
                .map(|f| unsafe { std::mem::transmute::<*mut c_void, GetFanSpeedRpm>(f) });
            let fan_percent_v2 = export(module, b"nvmlDeviceGetFanSpeed_v2\0")
                .map(|f| unsafe { std::mem::transmute::<*mut c_void, GetFanSpeedV2>(f) });
            let fan_percent = export(module, b"nvmlDeviceGetFanSpeed\0")
                .map(|f| unsafe { std::mem::transmute::<*mut c_void, GetFanSpeed>(f) });
            (!devices.is_empty()).then_some(Self {
                devices,
                get_temperature,
                num_fans,
                fan_rpm,
                fan_percent_v2,
                fan_percent,
            })
        }

        /// One fan: rpm when the driver reports it, else percent of maximum; `None` when the
        /// driver gives neither (a fanless card, a laptop, a failed call).
        fn fan(&self, device: Device, index: u32) -> Option<Fan> {
            if let Some(rpm) = self.fan_rpm {
                let mut info = FanSpeedInfo {
                    version: FAN_SPEED_INFO_VERSION,
                    fan: index,
                    speed: 0,
                };
                // SAFETY: a handle NVML returned and an in/out struct valid for the call.
                if unsafe { rpm(device, &mut info) } == 0 {
                    return Some(Fan::Rpm(info.speed));
                }
            }
            let mut percent = 0u32;
            let status = if let Some(v2) = self.fan_percent_v2 {
                // SAFETY: as above, with an out pointer valid for the call.
                unsafe { v2(device, index, &mut percent) }
            } else if let (Some(v1), 0) = (self.fan_percent, index) {
                // SAFETY: as above.
                unsafe { v1(device, &mut percent) }
            } else {
                return None;
            };
            (status == 0 && percent <= 100).then_some(Fan::Percent(percent))
        }

        fn sample(&self) -> Sample {
            let mut fans = Vec::new();
            for device in &self.devices {
                let mut count = 1u32;
                if let Some(num) = self.num_fans {
                    // SAFETY: a handle NVML returned and an out pointer valid for the call.
                    // A failed call leaves the guess of one fan; a fanless card then fails
                    // the speed calls and adds nothing.
                    let mut asked = 0u32;
                    if unsafe { num(*device, &mut asked) } == 0 {
                        count = asked;
                    }
                }
                for index in 0..count.min(MAX_FANS) {
                    if let Some(fan) = self.fan(*device, index) {
                        fans.push(fan);
                    }
                }
            }
            Sample {
                celsius: self.hottest(),
                fans,
            }
        }

        /// The hottest GPU's core temperature in degrees Celsius; `None` when none answers.
        fn hottest(&self) -> Option<u32> {
            self.devices
                .iter()
                .filter_map(|device| {
                    let mut degrees = 0u32;
                    // SAFETY: a handle NVML returned and an out pointer valid for the call.
                    let status = unsafe { (self.get_temperature)(*device, 0, &mut degrees) };
                    (status == 0).then_some(degrees)
                })
                .max()
        }
    }

    pub fn watch(every: Duration) -> i32 {
        let Some(nvml) = Nvml::open() else {
            return 2;
        };
        // The parent closes this process's input when it wants no more readings (or dies).
        std::thread::spawn(|| {
            let mut sink = [0u8; 64];
            let mut input = std::io::stdin();
            loop {
                if !matches!(input.read(&mut sink), Ok(read) if read > 0) {
                    std::process::exit(0);
                }
            }
        });
        let mut out = std::io::stdout();
        loop {
            let sample = nvml.sample();
            let celsius = sample
                .celsius
                .map_or_else(|| "none".to_string(), |c| c.to_string());
            let fans = if sample.fans.is_empty() {
                "-".to_string()
            } else {
                let tokens: Vec<String> = sample
                    .fans
                    .iter()
                    .map(|fan| match fan {
                        Fan::Rpm(n) => format!("r{n}"),
                        Fan::Percent(n) => format!("p{n}"),
                    })
                    .collect();
                tokens.join(",")
            };
            let line = format!("{celsius} {fans}");
            if writeln!(out, "{line}").is_err() || out.flush().is_err() {
                return 0;
            }
            std::thread::sleep(every);
        }
    }
}
