//! Host hardware probe and model fit estimate.
//!
//! Probe: macOS `sysctl` + `system_profiler -json`; Windows kernel32 FFI for
//! RAM and `reg.exe` display-class keys for GPUs; Linux `/proc/meminfo`;
//! `nvidia-smi` for NVIDIA VRAM on Windows/Linux when present.
//! `RIGHTKIT_HW_JSON` injects a profile (tests, or hardware of another host).
//!
//! Fit: `Measured` only when a recorded measurement names this hardware
//! profile; otherwise `Estimated` (requirement fits) or `Unsupported`
//! (requirement exceeds the usable pool - not a permanent limit), and
//! `Unqualified` when the model declares no requirement for this hardware class.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Gpu {
    pub name: String,
    #[serde(default)]
    pub vram_gb: Option<f64>,
    /// Shares memory with the CPU (Apple Silicon).
    #[serde(default)]
    pub unified: bool,
    #[serde(default)]
    pub backends: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hardware {
    pub os: String,
    pub arch: String,
    #[serde(default)]
    pub memory_gb: Option<f64>,
    #[serde(default)]
    pub gpus: Vec<Gpu>,
    #[serde(default)]
    pub profile: String,
    /// `detected` or `override`.
    #[serde(default)]
    pub source: String,
}

/// What a model needs, per hardware class. Missing = not declared.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Requirements {
    pub memory_gb: Option<f64>,
    pub unified_memory_gb: Option<f64>,
    pub vram_gb: Option<f64>,
}

/// A recorded real run on a named hardware profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measurement {
    pub profile: String,
    #[serde(default)]
    pub at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitKind {
    Measured,
    Estimated,
    Unsupported,
    Unqualified,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fit {
    pub kind: FitKind,
    pub profile: String,
    pub required_gb: Option<f64>,
    pub available_gb: Option<f64>,
    pub evidence: Option<Measurement>,
    pub reasons: Vec<String>,
}

impl Hardware {
    pub fn probe() -> Self {
        if let Ok(raw) = std::env::var("RIGHTKIT_HW_JSON") {
            if let Ok(mut hw) = serde_json::from_str::<Hardware>(&raw) {
                hw.source = "override".into();
                hw.profile = hw.profile_id();
                return hw;
            }
        }
        let mut hw = Hardware {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            memory_gb: platform_memory_bytes().map(|b| round1(b / 1_073_741_824.0)),
            gpus: detect_gpus(),
            profile: String::new(),
            source: "detected".into(),
        };
        hw.profile = hw.profile_id();
        hw
    }

    /// Stable profile id: `{os}-{mem}gb-unified`, `{os}-gpu-{vram}gb`, `{os}-{mem}gb`.
    pub fn profile_id(&self) -> String {
        let mem = self.memory_gb.unwrap_or(0.0).round() as i64;
        if let Some(g) = self.gpus.first() {
            if g.unified {
                return format!("{}-{mem}gb-unified", self.os);
            }
            if let Some(v) = g.vram_gb {
                return format!("{}-gpu-{}gb", self.os, v.round() as i64);
            }
        }
        format!("{}-{mem}gb", self.os)
    }

    fn unified(&self) -> bool {
        self.gpus.first().is_some_and(|g| g.unified)
    }

    fn max_vram(&self) -> f64 {
        self.gpus
            .iter()
            .filter(|g| !g.unified)
            .filter_map(|g| g.vram_gb)
            .fold(0.0, f64::max)
    }

    /// Conservative usable pool: two thirds of unified memory, the largest
    /// discrete VRAM, else half of system RAM.
    pub fn available_gb(&self) -> f64 {
        let mem = self.memory_gb.unwrap_or(0.0);
        if self.unified() {
            mem * 0.66
        } else if self.max_vram() > 0.0 {
            self.max_vram()
        } else {
            mem * 0.5
        }
    }

    pub fn required_gb(&self, req: &Requirements) -> Option<f64> {
        if self.unified() {
            req.unified_memory_gb.or(req.memory_gb).or(req.vram_gb)
        } else if self.max_vram() > 0.0 {
            req.vram_gb.or(req.memory_gb)
        } else {
            req.memory_gb.or(req.vram_gb)
        }
    }

    pub fn estimate_fit(&self, req: &Requirements, measured: &[Measurement]) -> Fit {
        let profile = self.profile.clone();
        let evidence = measured
            .iter()
            .find(|m| profiles_match(&m.profile, &profile))
            .cloned();
        let required = self.required_gb(req);
        let avail = round1(self.available_gb());
        let fit = |kind, reasons: Vec<String>, evidence| Fit {
            kind,
            profile: profile.clone(),
            required_gb: required,
            available_gb: Some(avail),
            evidence,
            reasons,
        };
        if evidence.is_some() {
            return fit(
                FitKind::Measured,
                vec!["measured on this hardware profile".into()],
                evidence,
            );
        }
        match required {
            None => Fit {
                kind: FitKind::Unqualified,
                profile: profile.clone(),
                required_gb: None,
                available_gb: Some(avail),
                evidence: None,
                reasons: vec!["no requirement data for this hardware class".into()],
            },
            Some(r) if r <= self.available_gb() => fit(
                FitKind::Estimated,
                vec!["requirement fits detected hardware; not yet measured here".into()],
                None,
            ),
            Some(r) => fit(
                FitKind::Unsupported,
                vec![format!(
                    "needs ~{r:.0} GB, ~{avail:.0} GB usable here; not a permanent limit"
                )],
                None,
            ),
        }
    }
}

/// Profile ids match exactly or modulo the `-unified` suffix, so a recorded
/// `macos-24gb` measurement applies to the detected `macos-24gb-unified`.
pub fn profiles_match(recorded: &str, detected: &str) -> bool {
    let strip = |s: &str| s.strip_suffix("-unified").unwrap_or(s).to_string();
    recorded == detected || strip(recorded) == strip(detected)
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(target_os = "macos")]
fn platform_memory_bytes() -> Option<f64> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(target_os = "windows")]
fn platform_memory_bytes() -> Option<f64> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut s = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    // SAFETY: `s` is a correctly sized, initialised MEMORYSTATUSEX.
    if unsafe { GlobalMemoryStatusEx(&mut s) } == 0 {
        return None;
    }
    Some(s.total_phys as f64)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_memory_bytes() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: f64 = text
        .lines()
        .find(|l| l.starts_with("MemTotal:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb * 1024.0)
}

#[cfg(target_os = "macos")]
fn detect_gpus() -> Vec<Gpu> {
    let mut gpus = Vec::new();
    if let Ok(out) = std::process::Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output()
    {
        if let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) {
            for g in v["SPDisplaysDataType"].as_array().into_iter().flatten() {
                let vendor = g["sppci_vendor"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                gpus.push(Gpu {
                    name: g["sppci_model"].as_str().unwrap_or("GPU").to_string(),
                    vram_gb: g["spdisplays_vram"]
                        .as_str()
                        .and_then(|s| s.split_whitespace().next())
                        .and_then(|s| s.parse().ok()),
                    unified: vendor.contains("apple") || std::env::consts::ARCH == "aarch64",
                    backends: vec!["metal".into()],
                });
            }
        }
    }
    if gpus.is_empty() && std::env::consts::ARCH == "aarch64" {
        gpus.push(Gpu {
            name: "Apple Silicon (unified)".into(),
            vram_gb: None,
            unified: true,
            backends: vec!["metal".into()],
        });
    }
    gpus
}

#[cfg(target_os = "windows")]
fn detect_gpus() -> Vec<Gpu> {
    const CLASS: &str =
        r"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let query = |value: &str| {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("reg")
            .args(["query", CLASS, "/s", "/v", value])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .ok()
    };
    let names: Vec<String> = query("DriverDesc")
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.contains("DriverDesc"))
                .filter_map(|l| l.split("REG_SZ").nth(1).map(|s| s.trim().to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mems: Vec<f64> = query("HardwareInformation.qwMemorySize")
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.contains("qwMemorySize"))
                .filter_map(|l| {
                    l.split("REG_QWORD").nth(1).and_then(|s| {
                        let t = s.trim();
                        t.strip_prefix("0x")
                            .or_else(|| t.strip_prefix("0X"))
                            .and_then(|h| u64::from_str_radix(h, 16).ok())
                            .or_else(|| t.parse().ok())
                    })
                })
                .map(|b| b as f64 / 1_073_741_824.0)
                .collect()
        })
        .unwrap_or_default();
    let mut gpus: Vec<Gpu> = names
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let nv = name.to_ascii_lowercase().contains("nvidia");
            Gpu {
                name,
                vram_gb: mems.get(i).copied().map(round1),
                unified: false,
                backends: if nv {
                    vec!["cuda".into(), "vulkan".into()]
                } else {
                    vec!["vulkan".into(), "directml".into()]
                },
            }
        })
        .collect();
    apply_nvidia_smi(&mut gpus);
    gpus
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn detect_gpus() -> Vec<Gpu> {
    let mut gpus = Vec::new();
    apply_nvidia_smi(&mut gpus);
    gpus
}

/// The registry qwMemorySize is capped for >4 GiB on some drivers; nvidia-smi
/// is authoritative for NVIDIA cards and is added when no entry covers them.
#[cfg(not(target_os = "macos"))]
fn apply_nvidia_smi(gpus: &mut Vec<Gpu>) {
    let mut nvidia_smi = std::process::Command::new("nvidia-smi");
    nvidia_smi.args([
        "--query-gpu=name,memory.total",
        "--format=csv,noheader,nounits",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        nvidia_smi.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: never flash a console from a GUI host
    }
    let Ok(out) = nvidia_smi.output() else {
        return;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((name, mib)) = line.rsplit_once(',') else {
            continue;
        };
        let (name, Ok(mib)) = (name.trim().to_string(), mib.trim().parse::<f64>()) else {
            continue;
        };
        let vram = round1(mib / 1024.0);
        match gpus.iter_mut().find(|g| g.name.eq_ignore_ascii_case(&name)) {
            Some(g) => g.vram_gb = Some(vram),
            None => gpus.push(Gpu {
                name,
                vram_gb: Some(vram),
                unified: false,
                backends: vec!["cuda".into(), "vulkan".into()],
            }),
        }
    }
}
