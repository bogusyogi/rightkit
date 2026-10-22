//! Host hardware probe and model fit estimate.
//!
//! Probe: macOS `sysctlbyname` + `system_profiler -json`; Windows kernel32 FFI for
//! RAM and `reg.exe` display-class keys for GPUs; Linux `/proc/meminfo`;
//! `nvidia-smi` for NVIDIA VRAM on Windows/Linux when present. [`probe`](crate::probe) runs the
//! same queries through an injected `probe::ProbeRunner` for confined callers.
//! `RIGHTKIT_HW_JSON` injects a profile (tests, or hardware of another host);
//! apps with their own override pass its path or JSON to
//! [`Hardware::probe_with`] (the crate never reads app env vars).
//! [`ProfileAlias`] files a detected host under an app-recorded profile id.
//!
//! Fit: `Measured` only when a recorded measurement names this hardware
//! profile; otherwise `Estimated` (requirement fits) or `Unsupported`
//! (requirement exceeds the usable pool - not a permanent limit), and
//! `Unqualified` when the model declares no requirement for this hardware class.
//! Models with no local backend ([`Requirements::remote_only`]) are
//! `RentalOnly` (unless measured on this profile) or `Api`.

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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
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
    /// Further profile ids this host answers to (see [`ProfileAlias`]);
    /// recorded measurements under any of them count as `Measured`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile_aliases: Vec<String>,
}

/// What a model needs, per hardware class. Missing = not declared.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Requirements {
    pub memory_gb: Option<f64>,
    pub unified_memory_gb: Option<f64>,
    pub vram_gb: Option<f64>,
    /// VRAM on a CUDA-capable (NVIDIA) GPU; preferred over `vram_gb` there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuda_vram_gb: Option<f64>,
    /// Memory for the Apple MPS/Metal backend on unified memory; preferred
    /// over `unified_memory_gb` there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mps_memory_gb: Option<f64>,
    /// The model has no local backend; hardware fit is `RentalOnly` / `Api`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_only: Option<RemoteRoute>,
}

/// Route of a model with no local backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRoute {
    /// Runs on a rented worker; a measurement on this profile still counts.
    Rental,
    /// Billed provider API; hardware fit does not apply.
    Api,
}

/// Files a host under an extra profile id when it matches: any GPU name
/// containing `gpu_name_contains` (case-insensitive), or a detected profile
/// equal to `profile` (modulo `-unified`). An alias with neither criterion
/// never matches. `primary` makes the alias the reported `profile` (the
/// detected id moves to `profile_aliases`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfileAlias {
    pub alias: String,
    #[serde(default)]
    pub gpu_name_contains: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub primary: bool,
}

/// An app-supplied hardware override (the crate never reads app env vars).
#[derive(Debug, Clone, Copy)]
pub enum HardwareOverride<'a> {
    /// Path to a JSON file.
    Path(&'a std::path::Path),
    /// The JSON text itself.
    Json(&'a str),
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
    /// No local backend; route through a rented worker.
    RentalOnly,
    /// No local backend; billed provider API.
    Api,
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
            if let Ok(hw) = Self::from_override_json(&raw) {
                return hw;
            }
        }
        Self::detect()
    }

    /// The real host, ignoring every override.
    pub fn detect() -> Self {
        let mut hw = Hardware {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            memory_gb: platform_memory_bytes().map(|b| round1(b / 1_073_741_824.0)),
            gpus: detect_gpus(),
            profile: String::new(),
            source: "detected".into(),
            profile_aliases: Vec::new(),
        };
        hw.profile = hw.profile_id();
        hw
    }

    /// `probe` with an app-supplied override taking precedence. `None` is
    /// exactly [`Hardware::probe`]; an unreadable or malformed override is an
    /// error (callers decide whether to fall back).
    pub fn probe_with(ov: Option<HardwareOverride<'_>>) -> crate::Result<Self> {
        match ov {
            None => Ok(Self::probe()),
            Some(HardwareOverride::Json(raw)) => Self::from_override_json(raw),
            Some(HardwareOverride::Path(path)) => {
                let raw = std::fs::read_to_string(path)
                    .map_err(|e| crate::Error::io(path.display(), e))?;
                Self::from_override_json(&raw)
            }
        }
    }

    /// Parse an injected profile leniently: missing `os`/`arch` take the
    /// host's, missing memory stays unknown, and unreadable `gpus` entries are
    /// dropped instead of rejecting the profile. `source` becomes `override`
    /// and `profile` is recomputed. Fails only when the text is not a JSON
    /// object or a present field has the wrong type.
    pub fn from_override_json(raw: &str) -> crate::Result<Self> {
        let bad = |why: String| {
            crate::Error::new(
                crate::ErrorCode::InvalidSpec,
                format!("hardware override: {why}"),
            )
        };
        let mut v: Value = serde_json::from_str(raw).map_err(|e| bad(e.to_string()))?;
        let obj = v
            .as_object_mut()
            .ok_or_else(|| bad("not a JSON object".into()))?;
        obj.entry("os")
            .or_insert_with(|| std::env::consts::OS.into());
        obj.entry("arch")
            .or_insert_with(|| std::env::consts::ARCH.into());
        let gpus: Vec<Value> = match obj.remove("gpus") {
            Some(Value::Array(list)) => list
                .into_iter()
                .filter(|g| serde_json::from_value::<Gpu>(g.clone()).is_ok())
                .collect(),
            _ => Vec::new(),
        };
        obj.insert("gpus".into(), Value::Array(gpus));
        let mut hw: Hardware = serde_json::from_value(v).map_err(|e| bad(e.to_string()))?;
        hw.source = "override".into();
        hw.profile = hw.profile_id();
        Ok(hw)
    }

    /// Apply every matching [`ProfileAlias`] (see its docs). Idempotent.
    pub fn apply_profile_aliases(&mut self, aliases: &[ProfileAlias]) {
        for a in aliases {
            if a.alias.is_empty() {
                continue;
            }
            let by_gpu = a.gpu_name_contains.as_deref().is_some_and(|needle| {
                let needle = needle.to_ascii_lowercase();
                !needle.is_empty()
                    && self
                        .gpus
                        .iter()
                        .any(|g| g.name.to_ascii_lowercase().contains(&needle))
            });
            let by_profile = a.profile.as_deref().is_some_and(|p| self.answers_to(p));
            if !(by_gpu || by_profile) {
                continue;
            }
            if a.primary && self.profile != a.alias {
                let old = std::mem::replace(&mut self.profile, a.alias.clone());
                self.profile_aliases.push(old);
            } else if self.profile != a.alias {
                self.profile_aliases.push(a.alias.clone());
            }
        }
        let current = self.profile.clone();
        self.profile_aliases.retain(|p| *p != current);
        let mut seen = std::collections::BTreeSet::new();
        self.profile_aliases.retain(|p| seen.insert(p.clone()));
    }

    /// Does a recorded profile id name this host (its profile or an alias)?
    pub fn answers_to(&self, recorded: &str) -> bool {
        profiles_match(recorded, &self.profile)
            || self
                .profile_aliases
                .iter()
                .any(|a| profiles_match(recorded, a))
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

    fn cuda(&self) -> bool {
        self.gpus.iter().any(|g| {
            !g.unified
                && (g.backends.iter().any(|b| b.eq_ignore_ascii_case("cuda"))
                    || g.name.to_ascii_lowercase().contains("nvidia"))
        })
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
            req.mps_memory_gb
                .or(req.unified_memory_gb)
                .or(req.memory_gb)
                .or(req.vram_gb)
        } else if self.max_vram() > 0.0 {
            if self.cuda() {
                req.cuda_vram_gb.or(req.vram_gb).or(req.memory_gb)
            } else {
                req.vram_gb.or(req.memory_gb)
            }
        } else {
            req.memory_gb.or(req.vram_gb)
        }
    }

    pub fn estimate_fit(&self, req: &Requirements, measured: &[Measurement]) -> Fit {
        let profile = self.profile.clone();
        let evidence = measured
            .iter()
            .find(|m| self.answers_to(&m.profile))
            .cloned();
        match req.remote_only {
            Some(RemoteRoute::Api) => {
                return Fit {
                    kind: FitKind::Api,
                    profile,
                    required_gb: None,
                    available_gb: None,
                    evidence: None,
                    reasons: vec!["billed provider route; hardware fit does not apply".into()],
                }
            }
            Some(RemoteRoute::Rental) if evidence.is_none() => {
                return Fit {
                    kind: FitKind::RentalOnly,
                    profile,
                    required_gb: None,
                    available_gb: None,
                    evidence: None,
                    reasons: vec![
                        "no local backend; route through a rented worker or provider API".into(),
                    ],
                }
            }
            _ => {}
        }
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

/// Physical memory in bytes, read in-process: `sysctlbyname("hw.memsize")` on macOS,
/// `GlobalMemoryStatusEx` on Windows and `/proc/meminfo` elsewhere. `None` when the platform does
/// not answer. No child process is started.
pub fn total_memory_bytes() -> Option<u64> {
    platform_memory_bytes().map(|bytes| bytes as u64)
}

/// Tiered memory needs of a local model, in GB: the smallest size that runs at all and the size
/// that runs well, for system RAM and, when the model is GPU-offloaded, for VRAM. Unlike
/// [`Requirements`], which gives one figure per hardware class, this drives [`Hardware::tiered_fit`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TieredRequirements {
    pub min_memory_gb: f64,
    pub recommended_memory_gb: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_vram_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_vram_gb: Option<f64>,
}

/// Verdict of [`Hardware::tiered_fit`]. `Good` needs every declared resource at its recommended
/// size; `Caution` means one is between minimum and recommended; `Unknown` means memory could not
/// be read or a declared VRAM target has no VRAM figure; `TooLarge` means a resource is below its
/// minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FitVerdict {
    Good,
    Caution,
    Unknown,
    TooLarge,
}

/// How far a [`FitVerdict`] can be trusted. `High` when the verdict used both a RAM and a VRAM
/// figure from the host, `Medium` when one of them was missing, `Low` when RAM is unknown and no
/// VRAM was reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FitConfidence {
    High,
    Medium,
    Low,
}

/// Result of [`Hardware::tiered_fit`]: the verdict, its confidence, a sentence for the user, and
/// the host figures it was based on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TieredFit {
    pub verdict: FitVerdict,
    pub confidence: FitConfidence,
    pub reason: String,
    pub detected_memory_gb: Option<f64>,
    pub detected_vram_gb: Option<f64>,
}

impl Hardware {
    /// Verdict for a model's tiered memory needs against this host. The host figures are its RAM
    /// and the largest VRAM any GPU reports. A recorded measurement is not consulted here; use
    /// [`Hardware::estimate_fit`] for `Measured`.
    pub fn tiered_fit(&self, req: &TieredRequirements) -> TieredFit {
        let vram = self
            .gpus
            .iter()
            .filter_map(|gpu| gpu.vram_gb)
            .reduce(f64::max);
        let memory = self.memory_gb;
        // A VRAM target is declared when either bound is given. A missing bound takes the other:
        // a minimum-only target is met exactly when it is met, with nothing between the bounds.
        let vram_target = req
            .min_vram_gb
            .or(req.recommended_vram_gb)
            .map(|min| (min, req.recommended_vram_gb.unwrap_or(min)));
        let vram_unmeasured = vram_target.is_some() && vram.is_none();
        let (verdict, reason, confidence) = match memory {
            None => (
                FitVerdict::Unknown,
                "Total memory could not be detected; verify RAM/VRAM manually.".to_string(),
                if vram.is_some() {
                    FitConfidence::Medium
                } else {
                    FitConfidence::Low
                },
            ),
            Some(total) => {
                let ram = band(total, req.min_memory_gb, req.recommended_memory_gb);
                let gpu = match (vram_target, vram) {
                    (Some((min, rec)), Some(v)) => Some((v, min, rec, band(v, min, rec))),
                    _ => None,
                };
                let confidence = if vram_unmeasured {
                    FitConfidence::Medium
                } else {
                    FitConfidence::High
                };
                let (verdict, reason) = match (ram, gpu) {
                    (Band::Below, _) => (
                        FitVerdict::TooLarge,
                        format!("Detected memory ({total:.1} GB) is below the estimated minimum."),
                    ),
                    (_, Some((v, min, _, Band::Below))) => (
                        FitVerdict::TooLarge,
                        format!(
                            "Detected VRAM ({v:.1} GB) is below the estimated minimum of {min:.1} GB."
                        ),
                    ),
                    _ if vram_unmeasured => (
                        FitVerdict::Unknown,
                        format!(
                            "Detected memory ({total:.1} GB) is known, but the model declares a VRAM target and no VRAM was detected; verify GPU memory manually."
                        ),
                    ),
                    (Band::Between, Some((v, min, rec, Band::Between))) => (
                        FitVerdict::Caution,
                        format!(
                            "Detected memory ({total:.1} GB) and VRAM ({v:.1} GB) are between the minimum and the recommended sizes ({min:.1} GB / {rec:.1} GB VRAM)."
                        ),
                    ),
                    (Band::Between, _) => (
                        FitVerdict::Caution,
                        format!(
                            "Detected memory ({total:.1} GB) meets the minimum but not the recommended size."
                        ),
                    ),
                    (_, Some((v, min, rec, Band::Between))) => (
                        FitVerdict::Caution,
                        format!(
                            "Detected VRAM ({v:.1} GB) meets the minimum ({min:.1} GB) but not the recommended {rec:.1} GB."
                        ),
                    ),
                    (_, Some((v, _, rec, _))) => (
                        FitVerdict::Good,
                        format!(
                            "Detected memory ({total:.1} GB) and VRAM ({v:.1} GB) meet the recommended sizes ({rec:.1} GB VRAM)."
                        ),
                    ),
                    (_, None) => (
                        FitVerdict::Good,
                        format!(
                            "Detected memory ({total:.1} GB) meets the recommended size; the model declares no VRAM target."
                        ),
                    ),
                };
                (verdict, reason, confidence)
            }
        };
        TieredFit {
            verdict,
            confidence,
            reason,
            detected_memory_gb: memory,
            detected_vram_gb: vram,
        }
    }
}

/// Where a host figure falls against a tier's minimum and recommended sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Band {
    Below,
    Between,
    Meets,
}

fn band(value: f64, minimum: f64, recommended: f64) -> Band {
    if value >= recommended {
        Band::Meets
    } else if value >= minimum {
        Band::Between
    } else {
        Band::Below
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
    use std::ffi::{c_char, c_int, c_void};
    extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }
    let mut bytes: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `hw.memsize` is a NUL-terminated name; `bytes` is an 8-byte buffer and `len` tells the
    // kernel so; no new value is set (`newp` is null and `newlen` is 0).
    let status = unsafe {
        sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut bytes as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && len == std::mem::size_of::<u64>()).then_some(bytes as f64)
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
    crate::android::meminfo_total_bytes(&text)
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

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "android")))]
fn detect_gpus() -> Vec<Gpu> {
    let mut gpus = Vec::new();
    apply_nvidia_smi(&mut gpus);
    gpus
}

/// Android has no discrete GPU to probe here (and no `nvidia-smi`); accelerated backends are not
/// part of the fit rules yet. Facts from the Android API come through `Hardware::from_android`.
#[cfg(target_os = "android")]
fn detect_gpus() -> Vec<Gpu> {
    Vec::new()
}

/// The registry qwMemorySize is capped for >4 GiB on some drivers; nvidia-smi
/// is authoritative for NVIDIA cards and is added when no entry covers them.
#[cfg(not(any(target_os = "macos", target_os = "android")))]
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

#[cfg(test)]
mod tiered_tests {
    use super::*;

    fn host(memory_gb: Option<f64>, vram_gb: Option<f64>) -> Hardware {
        let gpus = vram_gb
            .map(|vram| Gpu {
                name: "Test GPU".into(),
                vram_gb: Some(vram),
                unified: false,
                backends: vec!["vulkan".into()],
            })
            .into_iter()
            .collect();
        Hardware {
            os: "linux".into(),
            arch: "x86_64".into(),
            memory_gb,
            gpus,
            profile: String::new(),
            source: "override".into(),
            profile_aliases: Vec::new(),
        }
    }

    fn tier() -> TieredRequirements {
        TieredRequirements {
            min_memory_gb: 8.0,
            recommended_memory_gb: 16.0,
            min_vram_gb: Some(6.0),
            recommended_vram_gb: Some(8.0),
        }
    }

    fn outcome(hw: &Hardware, req: &TieredRequirements) -> (FitVerdict, FitConfidence) {
        let fit = hw.tiered_fit(req);
        (fit.verdict, fit.confidence)
    }

    #[test]
    fn ram_and_vram_pick_good_caution_or_too_large_with_a_confidence() {
        let req = tier();
        assert_eq!(
            outcome(&host(Some(32.0), Some(12.0)), &req),
            (FitVerdict::Good, FitConfidence::High)
        );
        assert_eq!(
            outcome(&host(Some(32.0), Some(7.0)), &req),
            (FitVerdict::Caution, FitConfidence::High)
        );
        assert_eq!(
            outcome(&host(Some(32.0), Some(4.0)), &req),
            (FitVerdict::TooLarge, FitConfidence::High)
        );
        assert_eq!(
            outcome(&host(Some(16.0), Some(8.0)), &req),
            (FitVerdict::Good, FitConfidence::High)
        );
        assert_eq!(
            outcome(&host(Some(10.0), Some(12.0)), &req),
            (FitVerdict::Caution, FitConfidence::High)
        );
        assert_eq!(
            outcome(&host(Some(10.0), None), &req),
            (FitVerdict::Unknown, FitConfidence::Medium)
        );
        assert_eq!(
            outcome(&host(Some(6.0), Some(12.0)), &req),
            (FitVerdict::TooLarge, FitConfidence::High)
        );
        assert_eq!(
            outcome(&host(Some(6.0), None), &req),
            (FitVerdict::TooLarge, FitConfidence::Medium)
        );
        assert_eq!(
            outcome(&host(Some(32.0), None), &req),
            (FitVerdict::Unknown, FitConfidence::Medium)
        );
    }

    #[test]
    fn strong_ram_with_vram_between_minimum_and_recommended_is_caution() {
        // 64 GB RAM and 8 GB VRAM against an 8 GB minimum / 16 GB recommended VRAM tier.
        let req = TieredRequirements {
            min_memory_gb: 16.0,
            recommended_memory_gb: 32.0,
            min_vram_gb: Some(8.0),
            recommended_vram_gb: Some(16.0),
        };
        let fit = host(Some(64.0), Some(8.0)).tiered_fit(&req);
        assert_eq!(fit.verdict, FitVerdict::Caution);
        assert_eq!(fit.confidence, FitConfidence::High);
        assert_eq!(
            outcome(&host(Some(64.0), Some(16.0)), &req),
            (FitVerdict::Good, FitConfidence::High)
        );
        assert_eq!(
            outcome(&host(Some(64.0), Some(4.0)), &req),
            (FitVerdict::TooLarge, FitConfidence::High)
        );
    }

    #[test]
    fn a_declared_vram_target_without_a_vram_reading_is_unknown() {
        let req = tier();
        // RAM is strong and VRAM is missing: Unknown, never Good.
        let fit = host(Some(64.0), None).tiered_fit(&req);
        assert_eq!(fit.verdict, FitVerdict::Unknown);
        assert_eq!(fit.confidence, FitConfidence::Medium);
        // A known RAM shortfall is still definitive even when VRAM is missing.
        assert_eq!(
            outcome(&host(Some(6.0), None), &req),
            (FitVerdict::TooLarge, FitConfidence::Medium)
        );
    }

    #[test]
    fn a_minimum_only_vram_target_is_met_or_not() {
        let req = TieredRequirements {
            min_memory_gb: 8.0,
            recommended_memory_gb: 16.0,
            min_vram_gb: Some(6.0),
            recommended_vram_gb: None,
        };
        assert_eq!(
            outcome(&host(Some(32.0), Some(6.0)), &req),
            (FitVerdict::Good, FitConfidence::High)
        );
        assert_eq!(
            outcome(&host(Some(32.0), Some(4.0)), &req),
            (FitVerdict::TooLarge, FitConfidence::High)
        );
    }

    #[test]
    fn an_unreadable_memory_is_unknown_with_confidence_from_vram_only() {
        let req = tier();
        assert_eq!(
            outcome(&host(None, None), &req),
            (FitVerdict::Unknown, FitConfidence::Low)
        );
        assert_eq!(
            outcome(&host(None, Some(8.0)), &req),
            (FitVerdict::Unknown, FitConfidence::Medium)
        );
    }

    #[test]
    fn a_model_without_vram_targets_fits_on_ram_alone() {
        let ram_only = TieredRequirements {
            min_memory_gb: 8.0,
            recommended_memory_gb: 16.0,
            ..TieredRequirements::default()
        };
        let fit = host(Some(32.0), Some(12.0)).tiered_fit(&ram_only);
        assert_eq!(fit.verdict, FitVerdict::Good);
        assert_eq!(fit.detected_vram_gb, Some(12.0));
        assert_eq!(fit.detected_memory_gb, Some(32.0));
    }
}
