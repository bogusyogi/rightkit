//! Bounded, read-only extended monitoring.
//!
//! OS-specific socket and power APIs are intentionally kept behind small
//! supplied-table adapters.  This keeps the core truthful when an adapter is
//! unavailable, denied, or cannot prove process identity.

use crate::{ProcessIdentity, ProcessInfo, procs};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;
use sysinfo::Networks;

/// Maximum number of samples retained for one process identity by default.
pub const DEFAULT_HISTORY_CAP: usize = 120;

/// Extended state includes Unknown because absence of a fact is distinct from
/// a provider that is known to be unavailable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ObservationCapability {
    Available,
    Unavailable,
    PermissionDenied,
    Unsupported,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation<T> {
    pub value: Option<T>,
    pub capability: ObservationCapability,
    pub label: String,
}

impl<T> Observation<T> {
    fn unavailable(label: impl Into<String>) -> Self {
        Self {
            value: None,
            capability: ObservationCapability::Unavailable,
            label: label.into(),
        }
    }

    fn unsupported(label: impl Into<String>) -> Self {
        Self {
            value: None,
            capability: ObservationCapability::Unsupported,
            label: label.into(),
        }
    }

    fn unknown(label: impl Into<String>) -> Self {
        Self {
            value: None,
            capability: ObservationCapability::Unknown,
            label: label.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NetworkCounters {
    pub received_bytes: u64,
    pub transmitted_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkRates {
    pub received_bytes_per_second: f64,
    pub transmitted_bytes_per_second: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub counters: Observation<NetworkCounters>,
    pub rates: Observation<NetworkRates>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkSample {
    /// Monotonic sample time supplied by caller. Milliseconds avoid wall-clock
    /// jumps and make persisted/fixture samples straightforward.
    pub timestamp_ms: u64,
    pub counters: NetworkCounters,
}

/// Computes rates from monotonically increasing OS counters. A decrease is
/// treated as a provider reset/restart, never as a huge rate.
#[derive(Clone, Debug, Default)]
pub struct NetworkRateSampler {
    previous: Option<NetworkSample>,
}

impl NetworkRateSampler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.previous = None;
    }

    pub fn sample_current(&mut self, timestamp_ms: u64) -> Observation<NetworkRates> {
        let Observation {
            value,
            capability,
            label,
        } = read_network_counters();
        match value {
            Some(counters) => self.sample(NetworkSample {
                timestamp_ms,
                counters,
            }),
            None => Observation {
                value: None,
                capability,
                label,
            },
        }
    }

    pub fn sample(&mut self, current: NetworkSample) -> Observation<NetworkRates> {
        let Some(previous) = self.previous.replace(current) else {
            return Observation::unknown("network rate requires two samples");
        };
        let Some(elapsed_ms) = current.timestamp_ms.checked_sub(previous.timestamp_ms) else {
            return Observation::unknown("network sample time moved backwards");
        };
        if elapsed_ms == 0 {
            return Observation::unknown("network sample interval is zero");
        }
        let Some(received) = current
            .counters
            .received_bytes
            .checked_sub(previous.counters.received_bytes)
        else {
            return Observation::unknown("received counter reset or wrapped");
        };
        let Some(transmitted) = current
            .counters
            .transmitted_bytes
            .checked_sub(previous.counters.transmitted_bytes)
        else {
            return Observation::unknown("transmitted counter reset or wrapped");
        };
        let seconds = elapsed_ms as f64 / 1_000.0;
        Observation {
            value: Some(NetworkRates {
                received_bytes_per_second: received as f64 / seconds,
                transmitted_bytes_per_second: transmitted as f64 / seconds,
            }),
            capability: ObservationCapability::Available,
            label: "OS network byte counters over monotonic interval".into(),
        }
    }
}

/// Loopback and virtual interfaces carry traffic that a physical interface
/// already counted (tunnels, bridges, virtual switches) or that never leaves the computer.
#[cfg(windows)]
fn is_virtual_interface(name: &str) -> bool {
    // Windows names adapters for people ("Ethernet", "Wi-Fi", "vEthernet (WSL)"), so the
    // virtual ones are found by word: the loopback, tunnel and pseudo adapters, the virtual
    // switches of WSL and Hyper-V, VM and VPN adapters, and Wi-Fi Direct's hidden adapters.
    const WORDS: [&str; 20] = [
        "loopback",
        "pseudo",
        "isatap",
        "teredo",
        "6to4",
        "vethernet",
        "wsl",
        "default switch",
        "hyper-v",
        "virtualbox",
        "vmware",
        "wi-fi direct",
        "local area connection*",
        "bluetooth",
        "wireguard",
        "tailscale",
        "tap-windows",
        "openvpn",
        "vpn",
        "npcap",
    ];
    let name = name.to_ascii_lowercase();
    WORDS.iter().any(|word| name.contains(word))
}

/// Loopback and virtual interfaces carry traffic that a physical interface
/// already counted (tunnels, AirDrop, bridges) or that never leaves the Mac.
#[cfg(not(windows))]
fn is_virtual_interface(name: &str) -> bool {
    const PREFIXES: [&str; 7] = ["lo", "utun", "awdl", "llw", "bridge", "anpi", "ap"];
    PREFIXES.iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Aggregate sysinfo's per-interface counters without allowing integer wrap.
/// Loopback and virtual interfaces are left out so traffic is not counted twice.
pub fn read_network_counters() -> Observation<NetworkCounters> {
    let networks = match Networks::new_with_refreshed_list() {
        Ok(networks) => networks,
        Err(error) => {
            return Observation::unavailable(format!(
                "OS refused the network interface list: {error}"
            ));
        }
    };
    if networks
        .keys()
        .all(|name| is_virtual_interface(&name.to_string_lossy()))
    {
        return Observation::unavailable("OS reported no physical network interfaces");
    }
    let mut counters = NetworkCounters::default();
    for (name, data) in &networks {
        if is_virtual_interface(&name.to_string_lossy()) {
            continue;
        }
        // `received`/`transmitted` are refresh deltas. Lifetime totals are
        // required here because this function creates a fresh Networks view.
        let Some(received) = counters.received_bytes.checked_add(data.total_received()) else {
            return Observation::unknown("received network counter overflow");
        };
        let Some(transmitted) = counters
            .transmitted_bytes
            .checked_add(data.total_transmitted())
        else {
            return Observation::unknown("transmitted network counter overflow");
        };
        counters.received_bytes = received;
        counters.transmitted_bytes = transmitted;
    }
    Observation {
        value: Some(counters),
        capability: ObservationCapability::Available,
        label: "OS network byte counters, aggregated by interface".into(),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatteryStatus {
    pub charge_percent: Observation<f32>,
    pub seconds_remaining: Observation<u64>,
    pub charging: Observation<bool>,
    /// Signed watts: positive charging, negative discharging.
    pub watts: Observation<f32>,
    pub celsius: Observation<f32>,
}

/// Portable input for native power adapters. `charge_percent` is a percentage,
/// never a 0..1 fraction; invalid units become Unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NativeBatteryReading {
    pub charge_percent: Option<f64>,
    pub seconds_remaining: Option<u64>,
    pub charging: Option<bool>,
    /// Signed watts: positive charging, negative discharging.
    pub watts: Option<f64>,
    pub celsius: Option<f64>,
}

pub fn normalize_battery(reading: NativeBatteryReading) -> BatteryStatus {
    let charge_percent = match reading.charge_percent {
        Some(value) if value.is_finite() && (0.0..=100.0).contains(&value) => Observation {
            value: Some(value as f32),
            capability: ObservationCapability::Available,
            label: "battery charge percentage".into(),
        },
        Some(_) => Observation::unknown("battery charge has invalid percentage units"),
        None => Observation::unknown("battery charge was not supplied by native adapter"),
    };
    let seconds_remaining = reading
        .seconds_remaining
        .map(|value| Observation {
            value: Some(value),
            capability: ObservationCapability::Available,
            label: "battery estimated seconds remaining".into(),
        })
        .unwrap_or_else(|| Observation::unknown("battery time remaining unavailable"));
    let charging = reading
        .charging
        .map(|value| Observation {
            value: Some(value),
            capability: ObservationCapability::Available,
            label: "battery charging state".into(),
        })
        .unwrap_or_else(|| Observation::unknown("battery charging state unavailable"));
    let watts = reading
        .watts
        .filter(|value| value.is_finite())
        .map(|value| Observation {
            value: Some(value as f32),
            capability: ObservationCapability::Available,
            label: "battery power in watts (positive charging)".into(),
        })
        .unwrap_or_else(|| {
            Observation::unknown("battery power was not supplied by native adapter")
        });
    let celsius = reading
        .celsius
        .filter(|value| value.is_finite() && (-20.0..=100.0).contains(value))
        .map(|value| Observation {
            value: Some(value as f32),
            capability: ObservationCapability::Available,
            label: "battery temperature in degrees Celsius".into(),
        })
        .unwrap_or_else(|| {
            Observation::unknown("battery temperature was not supplied by native adapter")
        });
    BatteryStatus {
        charge_percent,
        seconds_remaining,
        charging,
        watts,
        celsius,
    }
}

pub fn unsupported_battery() -> BatteryStatus {
    BatteryStatus {
        charge_percent: Observation::unsupported("battery provider is not wired on this target"),
        seconds_remaining: Observation::unsupported("battery provider is not wired on this target"),
        charging: Observation::unsupported("battery provider is not wired on this target"),
        watts: Observation::unsupported("battery provider is not wired on this target"),
        celsius: Observation::unsupported("battery provider is not wired on this target"),
    }
}

#[cfg(windows)]
#[repr(C)]
struct NativeSystemPowerStatus {
    ac_line_status: u8,
    battery_flag: u8,
    battery_life_percent: u8,
    system_status_flag: u8,
    battery_life_time: u32,
    battery_full_life_time: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemPowerStatus(status: *mut NativeSystemPowerStatus) -> i32;
}

/// Read Windows' bounded power summary without spawning a process. Windows
/// uses 255 and UINT32_MAX as unknown sentinels; these become Unknown.
#[cfg(windows)]
pub fn read_battery() -> BatteryStatus {
    let mut raw = std::mem::MaybeUninit::<NativeSystemPowerStatus>::uninit();
    let ok = unsafe { GetSystemPowerStatus(raw.as_mut_ptr()) } != 0;
    if !ok {
        return BatteryStatus {
            charge_percent: Observation::unavailable("GetSystemPowerStatus failed"),
            seconds_remaining: Observation::unavailable("GetSystemPowerStatus failed"),
            charging: Observation::unavailable("GetSystemPowerStatus failed"),
            watts: Observation::unavailable("GetSystemPowerStatus failed"),
            celsius: Observation::unavailable("GetSystemPowerStatus failed"),
        };
    }
    let raw = unsafe { raw.assume_init() };
    let device = battery_device();
    let mut status = normalize_battery(NativeBatteryReading {
        charge_percent: (raw.battery_life_percent != 255)
            .then_some(raw.battery_life_percent as f64),
        seconds_remaining: (raw.battery_life_time != u32::MAX)
            .then_some(raw.battery_life_time as u64),
        charging: match raw.ac_line_status {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        watts: device.watts,
        celsius: device.celsius,
    });
    // A battery that never answers the driver query says why, rather than "not supplied".
    if device.watts.is_none() {
        status.watts = Observation::unavailable(
            "this battery's driver gives no power rate (or reports it in relative units)",
        );
    }
    if device.celsius.is_none() {
        status.celsius = Observation::unavailable(
            "Windows exposes battery temperature only when the battery driver supports it, and this one does not",
        );
    }
    status
}

#[cfg(windows)]
#[derive(Clone, Copy, Default)]
struct BatteryDevice {
    watts: Option<f64>,
    celsius: Option<f64>,
}

/// The battery driver's own readings (`IOCTL_BATTERY_QUERY_STATUS` rate in mW, signed;
/// `BatteryTemperature` in tenths of a kelvin when supported), at most once every 30 seconds:
/// finding the device walks the setup database. Direct driver calls, no subprocess. The
/// notch has its own copy (`windows/src/sensors.rs`); it does not depend on this crate.
#[cfg(windows)]
fn battery_device() -> BatteryDevice {
    use std::sync::{Mutex, PoisonError};
    use std::time::Instant;
    static LAST: Mutex<Option<(Instant, BatteryDevice)>> = Mutex::new(None);
    const KEEP: Duration = Duration::from_secs(30);
    if let Some((at, device)) = *LAST.lock().unwrap_or_else(PoisonError::into_inner)
        && at.elapsed() < KEEP
    {
        return device;
    }
    let device = query_battery_device();
    *LAST.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), device));
    device
}

#[cfg(windows)]
fn battery_ioctl<I, O>(
    handle: windows::Win32::Foundation::HANDLE,
    code: u32,
    input: &I,
    output: &mut O,
) -> bool {
    use windows::Win32::System::IO::DeviceIoControl;
    let mut returned = 0u32;
    // SAFETY: both buffers are live, plain-data values of the sizes passed.
    unsafe {
        DeviceIoControl(
            handle,
            code,
            Some((input as *const I).cast()),
            std::mem::size_of::<I>() as u32,
            Some((output as *mut O).cast()),
            std::mem::size_of::<O>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .is_ok()
}

#[cfg(windows)]
fn query_battery_device() -> BatteryDevice {
    use std::mem::size_of;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, GUID_DEVCLASS_BATTERY, SP_DEVICE_INTERFACE_DATA,
        SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList,
        SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
    };
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::core::PCWSTR;
    // SAFETY: SetupAPI enumeration with a valid class GUID; the list is destroyed below and
    // every buffer handed to the calls is sized from the call's own answer.
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_BATTERY),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        ) else {
            return BatteryDevice::default();
        };
        let mut found = BatteryDevice::default();
        for index in 0..8u32 {
            let mut data = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(set, None, &GUID_DEVCLASS_BATTERY, index, &mut data)
                .is_err()
            {
                break;
            }
            let mut needed = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut needed), None);
            let header = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            if (needed as usize) < header || needed > 4096 {
                continue;
            }
            // u32 backing keeps the detail structure aligned.
            let mut buffer = vec![0u32; (needed as usize).div_ceil(4)];
            let detail = buffer
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = header as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &data, Some(detail), needed, None, None)
                .is_err()
            {
                continue;
            }
            let path = PCWSTR(std::ptr::addr_of!((*detail).DevicePath).cast());
            let Ok(handle) = CreateFileW(
                path,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            ) else {
                continue;
            };
            let device = read_battery_handle(handle);
            let _ = CloseHandle(handle);
            if let Some(device) = device {
                found = device;
                break;
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        found
    }
}

#[cfg(windows)]
fn read_battery_handle(handle: windows::Win32::Foundation::HANDLE) -> Option<BatteryDevice> {
    use windows::Win32::System::Power::{
        BATTERY_CAPACITY_RELATIVE, BATTERY_INFORMATION, BATTERY_QUERY_INFORMATION, BATTERY_STATUS,
        BATTERY_WAIT_STATUS, BatteryInformation, BatteryTemperature,
        IOCTL_BATTERY_QUERY_INFORMATION, IOCTL_BATTERY_QUERY_STATUS, IOCTL_BATTERY_QUERY_TAG,
    };
    let mut tag = 0u32;
    if !battery_ioctl(handle, IOCTL_BATTERY_QUERY_TAG, &0u32, &mut tag) || tag == 0 {
        return None;
    }
    let wait = BATTERY_WAIT_STATUS {
        BatteryTag: tag,
        Timeout: 0,
        PowerState: 0,
        LowCapacity: 0,
        HighCapacity: 0,
    };
    let mut status = BATTERY_STATUS::default();
    if !battery_ioctl(handle, IOCTL_BATTERY_QUERY_STATUS, &wait, &mut status) {
        return None;
    }
    let query = |level| BATTERY_QUERY_INFORMATION {
        BatteryTag: tag,
        InformationLevel: level,
        AtRate: 0,
    };
    let mut info = BATTERY_INFORMATION::default();
    // A battery reporting relative capacity has no rate in milliwatts.
    let relative = !battery_ioctl(
        handle,
        IOCTL_BATTERY_QUERY_INFORMATION,
        &query(BatteryInformation),
        &mut info,
    ) || info.Capabilities & BATTERY_CAPACITY_RELATIVE != 0;
    // BATTERY_UNKNOWN_RATE is 0x80000000, which is i32::MIN as the signed rate.
    let watts = (!relative && status.Rate != i32::MIN).then(|| f64::from(status.Rate) / 1000.0);
    let mut tenth_kelvin = 0u32;
    let celsius = battery_ioctl(
        handle,
        IOCTL_BATTERY_QUERY_INFORMATION,
        &query(BatteryTemperature),
        &mut tenth_kelvin,
    )
    .then(|| f64::from(tenth_kelvin) / 10.0 - 273.15);
    Some(BatteryDevice { watts, celsius })
}

/// macOS: read `pmset -g batt` (the system's own power summary, no private API), at most once
/// every 30 seconds: the monitor samples far more often, and a process per sample would cost the
/// notch's idle CPU budget for a figure that changes slowly.
#[cfg(target_os = "macos")]
pub fn read_battery() -> BatteryStatus {
    use std::sync::{Mutex, PoisonError};
    use std::time::Instant;
    static LAST: Mutex<Option<(Instant, BatteryStatus)>> = Mutex::new(None);
    const KEEP: Duration = Duration::from_secs(30);
    if let Some((at, status)) = LAST.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        && at.elapsed() < KEEP
    {
        return status.clone();
    }
    let status = read_battery_now();
    *LAST.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), status.clone()));
    status
}

#[cfg(target_os = "macos")]
fn read_battery_now() -> BatteryStatus {
    use rightkit_child_process::RunOptions;
    let options = RunOptions::new(Duration::from_secs(3))
        .stdout_cap(16 * 1024)
        .stderr_cap(0);
    match rightkit_child_process::run("/usr/bin/pmset", ["-g", "batt"], &options) {
        Ok(done) if !done.timed_out && done.status.success() => {
            let text = String::from_utf8_lossy(&done.stdout.bytes);
            match parse_pmset_batt(&text) {
                Some(reading) => normalize_battery(reading),
                None => BatteryStatus {
                    charge_percent: Observation::unavailable(
                        "this Mac reports no internal battery",
                    ),
                    seconds_remaining: Observation::unavailable(
                        "this Mac reports no internal battery",
                    ),
                    charging: Observation::unavailable("this Mac reports no internal battery"),
                    watts: Observation::unavailable("this Mac reports no internal battery"),
                    celsius: Observation::unavailable("this Mac reports no internal battery"),
                },
            }
        }
        _ => BatteryStatus {
            charge_percent: Observation::unavailable("pmset -g batt did not answer"),
            seconds_remaining: Observation::unavailable("pmset -g batt did not answer"),
            charging: Observation::unavailable("pmset -g batt did not answer"),
            watts: Observation::unavailable("pmset -g batt did not answer"),
            celsius: Observation::unavailable("pmset -g batt did not answer"),
        },
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn read_battery() -> BatteryStatus {
    unsupported_battery()
}

/// Parse the `InternalBattery` line of `pmset -g batt`. None when there is no such line.
/// Time remaining is reported only when pmset gives a concrete `H:MM remaining`; "(no
/// estimate)" stays Unknown.
#[cfg(target_os = "macos")]
fn parse_pmset_batt(text: &str) -> Option<NativeBatteryReading> {
    let line = text.lines().find(|line| line.contains("InternalBattery"))?;
    let percent = line.split('%').next().and_then(|head| {
        let digits: String = head
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        digits.parse::<f64>().ok()
    });
    let lower = line.to_ascii_lowercase();
    let charging = if lower.contains("discharging") {
        Some(false)
    } else if lower.contains("charging") && !lower.contains("not charging") {
        Some(true)
    } else if lower.contains("charged") || lower.contains("not charging") {
        Some(false)
    } else {
        None
    };
    let seconds_remaining = lower.find(" remaining").and_then(|end| {
        let head = &lower[..end];
        let token = head
            .rsplit(|c: char| c.is_whitespace() || c == ';')
            .next()?;
        let (hours, minutes) = token.split_once(':')?;
        Some(hours.parse::<u64>().ok()? * 3600 + minutes.parse::<u64>().ok()? * 60)
    });
    Some(NativeBatteryReading {
        charge_percent: percent,
        seconds_remaining,
        charging,
        ..NativeBatteryReading::default()
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeListener {
    pub protocol: String,
    pub local_address: String,
    pub port: u16,
    pub pid: Option<u32>,
    /// Required for safe association. A PID alone is not a process identity.
    pub start_time: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListeningPort {
    pub protocol: String,
    pub local_address: String,
    pub port: u16,
    pub owner: Option<ProcessIdentity>,
    pub capability: ObservationCapability,
    pub reason: String,
}

/// Associate native socket rows only when PID and process start time both
/// match. This rejects PID reuse and missing-owner ambiguity.
pub fn normalize_listeners(
    rows: &[NativeListener],
    processes: &[ProcessIdentity],
) -> Vec<ListeningPort> {
    let identities: BTreeMap<u32, Vec<&ProcessIdentity>> =
        processes.iter().fold(BTreeMap::new(), |mut map, identity| {
            map.entry(identity.pid).or_default().push(identity);
            map
        });
    let mut normalized = rows
        .iter()
        .map(|row| {
            let (owner, capability, reason) = match (row.pid, row.start_time) {
                (None, _) => (
                    None,
                    ObservationCapability::Unknown,
                    "owner PID unavailable",
                ),
                (Some(_), None) => (
                    None,
                    ObservationCapability::Unknown,
                    "owner start time unavailable; PID reuse cannot be excluded",
                ),
                (Some(pid), Some(start_time)) => match identities.get(&pid) {
                    Some(found) if found.iter().any(|item| item.start_time == start_time) => (
                        Some(ProcessIdentity { pid, start_time }),
                        ObservationCapability::Available,
                        "owner matched by PID and start time",
                    ),
                    Some(_) => (
                        None,
                        ObservationCapability::Unknown,
                        "owner PID exists but start time does not match",
                    ),
                    None => (
                        None,
                        ObservationCapability::Unknown,
                        "owner process was not present in process snapshot",
                    ),
                },
            };
            ListeningPort {
                protocol: row.protocol.clone(),
                local_address: row.local_address.clone(),
                port: row.port,
                owner,
                capability,
                reason: reason.into(),
            }
        })
        .collect::<Vec<_>>();
    normalized.sort_by(|left, right| {
        (
            &left.protocol,
            &left.local_address,
            left.port,
            left.owner.as_ref().map(|identity| identity.pid),
        )
            .cmp(&(
                &right.protocol,
                &right.local_address,
                right.port,
                right.owner.as_ref().map(|identity| identity.pid),
            ))
    });
    normalized
}

/// Windows owner-table rows (IP Helper `MIB_TCPROW_OWNER_PID` and `MIB_TCP6ROW_OWNER_PID`).
/// Ports are in network byte order in the low 16 bits; addresses are in network byte order.
#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct TcpOwnerRow4 {
    state: u32,
    local_addr: u32,
    local_port: u32,
    remote_addr: u32,
    remote_port: u32,
    owner: u32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct TcpOwnerRow6 {
    local_addr: [u8; 16],
    local_scope: u32,
    local_port: u32,
    remote_addr: [u8; 16],
    remote_scope: u32,
    remote_port: u32,
    state: u32,
    owner: u32,
}

#[cfg(windows)]
const AF_INET: u32 = 2;
#[cfg(windows)]
const AF_INET6: u32 = 23;
/// `TCP_TABLE_OWNER_PID_LISTENER`: listening TCP endpoints with their owner PID.
#[cfg(windows)]
const TCP_TABLE_OWNER_PID_LISTENER: u32 = 3;
#[cfg(windows)]
const MIB_TCP_STATE_LISTEN: u32 = 2;
#[cfg(windows)]
const NO_ERROR: u32 = 0;
#[cfg(windows)]
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

#[cfg(windows)]
#[link(name = "iphlpapi")]
#[allow(non_snake_case)]
unsafe extern "system" {
    // Same signature as `process_inspect`'s declaration (the lint refuses two that differ).
    fn GetExtendedTcpTable(
        table: *mut std::ffi::c_void,
        size: *mut u32,
        order: i32,
        family: u32,
        class: u32,
        reserved: u32,
    ) -> u32;
}

/// One owner table for one address family, sized by the API's own answer. The table can grow
/// between the sizing call and the read, so the read is retried a bounded number of times.
#[cfg(windows)]
fn owner_table(family: u32) -> Result<Vec<u8>, u32> {
    let mut size: u32 = 0;
    let probe = unsafe {
        GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if probe != NO_ERROR && probe != ERROR_INSUFFICIENT_BUFFER {
        return Err(probe);
    }
    for _ in 0..4 {
        let mut buffer = vec![0u8; size as usize];
        let code = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut size,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if code == NO_ERROR {
            return Ok(buffer);
        }
        if code != ERROR_INSUFFICIENT_BUFFER {
            return Err(code);
        }
    }
    Err(ERROR_INSUFFICIENT_BUFFER)
}

/// The rows of an owner table: a `u32` count, then the rows. The count is capped by the buffer
/// length so a short or damaged buffer cannot be read past its end.
#[cfg(windows)]
fn table_rows<T: Copy>(bytes: &[u8]) -> Vec<T> {
    let size = std::mem::size_of::<T>();
    if bytes.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let available = (bytes.len() - 4) / size;
    (0..count.min(available))
        .map(|i| {
            let offset = 4 + i * size;
            // SAFETY: the row lies inside `bytes` (bounded by `available`); the read is unaligned.
            unsafe { std::ptr::read_unaligned(bytes.as_ptr().add(offset).cast::<T>()) }
        })
        .collect()
}

/// Listening TCP sockets of every process, with owner PIDs from the IP Helper owner tables. The
/// table carries no process start time, so each PID takes its start time from `processes`, the
/// snapshot the caller also holds. A PID with two identities in that snapshot gets none and is
/// reported Unknown by `normalize_listeners`. A PID reused between the two reads is not excluded.
#[cfg(windows)]
pub fn read_listening_ports(processes: &[ProcessIdentity]) -> Observation<Vec<ListeningPort>> {
    use std::net::{Ipv4Addr, Ipv6Addr};
    let start_of = |pid: u32| -> Option<u64> {
        let mut matches = processes.iter().filter(|identity| identity.pid == pid);
        let first = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(first.start_time)
        }
    };
    let mut natives = Vec::new();
    let v4 = match owner_table(AF_INET) {
        Ok(bytes) => bytes,
        Err(code) => {
            return Observation::unavailable(format!(
                "IP Helper TCP table unavailable (Windows error {code})"
            ));
        }
    };
    for row in table_rows::<TcpOwnerRow4>(&v4) {
        if row.state != MIB_TCP_STATE_LISTEN {
            continue;
        }
        natives.push(NativeListener {
            protocol: "tcp".into(),
            local_address: Ipv4Addr::from(row.local_addr.to_ne_bytes()).to_string(),
            port: u16::from_be(row.local_port as u16),
            pid: Some(row.owner),
            start_time: start_of(row.owner),
        });
    }
    let v6 = match owner_table(AF_INET6) {
        Ok(bytes) => bytes,
        Err(code) => {
            return Observation::unavailable(format!(
                "IP Helper TCP6 table unavailable (Windows error {code})"
            ));
        }
    };
    for row in table_rows::<TcpOwnerRow6>(&v6) {
        if row.state != MIB_TCP_STATE_LISTEN {
            continue;
        }
        natives.push(NativeListener {
            protocol: "tcp".into(),
            local_address: Ipv6Addr::from(row.local_addr).to_string(),
            port: u16::from_be(row.local_port as u16),
            pid: Some(row.owner),
            start_time: start_of(row.owner),
        });
    }
    Observation {
        value: Some(normalize_listeners(&natives, processes)),
        capability: ObservationCapability::Available,
        label: "TCP listeners by owner PID; start times from the same process snapshot".into(),
    }
}

/// macOS has no owner-table API in this build; the socket walk through libproc is not wired.
#[cfg(not(windows))]
pub fn read_listening_ports(_processes: &[ProcessIdentity]) -> Observation<Vec<ListeningPort>> {
    Observation::unsupported("listening socket provider is not wired on this target")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceSample {
    pub sampled_at_ms: u64,
    pub identity: ProcessIdentity,
    pub name: String,
    pub cpu_usage_percent: f32,
    pub memory_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceHistory {
    capacity_per_process: usize,
    samples: BTreeMap<(u32, u64), Vec<ResourceSample>>,
}

impl ResourceHistory {
    pub fn new(capacity_per_process: usize) -> Self {
        Self {
            capacity_per_process: capacity_per_process.max(1),
            samples: BTreeMap::new(),
        }
    }

    pub fn capacity_per_process(&self) -> usize {
        self.capacity_per_process
    }

    pub fn push(&mut self, sample: ResourceSample) {
        let key = (sample.identity.pid, sample.identity.start_time);
        let entries = self.samples.entry(key).or_default();
        entries.push(sample);
        if entries.len() > self.capacity_per_process {
            let excess = entries.len() - self.capacity_per_process;
            entries.drain(0..excess);
        }
    }

    pub fn push_processes(&mut self, sampled_at_ms: u64, processes: &[ProcessInfo]) {
        for process in processes {
            if process.cpu_usage_percent.is_finite()
                && process.cpu_usage_percent >= 0.0
                && let Some(memory_bytes) = process.memory.value
            {
                self.push(ResourceSample {
                    sampled_at_ms,
                    identity: process.identity.clone(),
                    name: process.name.clone(),
                    cpu_usage_percent: process.cpu_usage_percent,
                    memory_bytes,
                });
            }
        }
    }

    pub fn get(&self, identity: &ProcessIdentity) -> Option<&[ResourceSample]> {
        self.samples
            .get(&(identity.pid, identity.start_time))
            .map(Vec::as_slice)
    }

    pub fn process_count(&self) -> usize {
        self.samples.len()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtendedStatus {
    pub network: NetworkStatus,
    pub battery: BatteryStatus,
    pub listening_ports: Observation<Vec<ListeningPort>>,
    pub processes: Vec<ProcessInfo>,
}

/// Perform bounded real OS reads currently available through shared core APIs.
/// Battery reads are target-native; the socket provider is native on Windows and an explicit
/// unsupported adapter on macOS until its libproc walk can prove process identity.
pub fn sample_extended() -> ExtendedStatus {
    sample_extended_with(procs())
}

/// `sample_extended` for a one-shot monitor read, whose process rows show GPU use: on Windows
/// the GPU reading is taken twice, about a second apart (#380). The engine's poll uses `sample_extended`.
pub fn sample_extended_gpu_settled() -> ExtendedStatus {
    sample_extended_with(crate::procs_gpu_settled())
}

fn sample_extended_with(processes: Vec<ProcessInfo>) -> ExtendedStatus {
    let counters = read_network_counters();
    let identities: Vec<ProcessIdentity> = processes
        .iter()
        .map(|process| process.identity.clone())
        .collect();
    let listening_ports = read_listening_ports(&identities);
    let rates = Observation::unavailable("network rate requires caller-owned sampler state");
    ExtendedStatus {
        network: NetworkStatus { counters, rates },
        battery: read_battery(),
        listening_ports,
        processes,
    }
}

/// Converts a standard duration to a bounded sample interval. Kept public for
/// native adapters that use `Duration` while avoiding division by zero.
pub fn duration_millis(duration: Duration) -> Option<u64> {
    u64::try_from(duration.as_millis())
        .ok()
        .filter(|value| *value > 0)
}
