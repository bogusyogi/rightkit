//! Injectable child-process probes for hardware and local-runtime detection.
//!
//! [`Hardware::detect`] starts `nvidia-smi`, `system_profiler` and `reg` itself. A caller that must
//! launch every child through its own confinement (a sandboxed process adapter, a daemon policy)
//! implements [`ProbeRunner`] and calls [`Hardware::probe_with_runner`] or
//! [`detect_inference_runtimes`]. The parsers are pure and work on captured output.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::hardware::{total_memory_bytes, Gpu, Hardware};

const GIB: f64 = 1_073_741_824.0;

/// The future a [`ProbeRunner`] returns.
pub type ProbeFuture<'a> = Pin<Box<dyn Future<Output = Option<(String, i32)>> + Send + 'a>>;

/// Runs one short probe child for hardware and runtime detection.
///
/// `run` resolves to the child's stdout (lossy UTF-8) and its exit status, or to `None` when the
/// program could not be started, its output could not be read, or it was cut short. The
/// implementation owns confinement and timing: this crate has no async runtime, so it cannot bound
/// a probe itself. Each call must return within the caller's per-probe budget (CodeRight uses
/// 1.2 s).
pub trait ProbeRunner: Sync {
    fn run<'a>(&'a self, program: &'a str, args: &'a [&'a str]) -> ProbeFuture<'a>;
}

/// A local inference runtime that [`detect_inference_runtimes`] looks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceRuntime {
    /// Stable id: `ollama`, `llama-cpp` or `vllm`.
    pub id: String,
    pub name: String,
    /// The program answered `--version` with exit status 0.
    pub detected: bool,
    /// Trimmed stdout of `--version`, when detected.
    pub version: Option<String>,
    /// How to install the runtime, or how to point an app at it.
    pub install_hint: String,
}

/// Look for Ollama, llama.cpp's `llama-server` and vLLM by running `<program> --version` through
/// `runner`. Every runtime is reported, detected or not, in a fixed order.
pub async fn detect_inference_runtimes(runner: &dyn ProbeRunner) -> Vec<InferenceRuntime> {
    const RUNTIMES: [(&str, &str, &str, &str); 3] = [
        (
            "ollama",
            "Ollama",
            "ollama",
            "Install Ollama and pull a coding model, then use http://127.0.0.1:11434/v1.",
        ),
        (
            "llama-cpp",
            "llama.cpp server",
            "llama-server",
            "Install llama.cpp and run llama-server with an OpenAI-compatible endpoint.",
        ),
        (
            "vllm",
            "vLLM",
            "vllm",
            "Install vLLM and expose an OpenAI-compatible server.",
        ),
    ];
    let mut found = Vec::with_capacity(RUNTIMES.len());
    for (id, name, program, hint) in RUNTIMES {
        let version = match runner.run(program, &["--version"]).await {
            Some((text, 0)) => Some(text.trim().to_string()),
            _ => None,
        };
        found.push(InferenceRuntime {
            id: id.into(),
            name: name.into(),
            detected: version.is_some(),
            version,
            install_hint: hint.into(),
        });
    }
    found
}

impl Hardware {
    /// Probe the host with every child process started through `runner`: `nvidia-smi` for NVIDIA
    /// cards, then the platform's own display query (`wmic` on Windows, `system_profiler` on
    /// macOS). RAM is read in-process ([`total_memory_bytes`]). The registry read in
    /// [`Hardware::detect`] is not repeated here. The result has source `detected`.
    pub async fn probe_with_runner(runner: &dyn ProbeRunner) -> Hardware {
        let mut gpus = nvidia_smi_gpus(runner).await;
        gpus.extend(platform_gpus(runner).await);
        let mut hw = Hardware {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            memory_gb: total_memory_bytes().map(|bytes| round1(bytes as f64 / GIB)),
            gpus: dedupe_gpus(gpus),
            profile: String::new(),
            source: "detected".into(),
            profile_aliases: Vec::new(),
        };
        hw.profile = hw.profile_id();
        hw
    }
}

async fn nvidia_smi_gpus(runner: &dyn ProbeRunner) -> Vec<Gpu> {
    let args = [
        "--query-gpu=name,memory.total",
        "--format=csv,noheader,nounits",
    ];
    match runner.run("nvidia-smi", &args).await {
        Some((text, 0)) => parse_nvidia_smi_csv(&text),
        _ => Vec::new(),
    }
}

#[cfg(target_os = "windows")]
async fn platform_gpus(runner: &dyn ProbeRunner) -> Vec<Gpu> {
    let args = [
        "path",
        "win32_VideoController",
        "get",
        "Name,AdapterRAM",
        "/format:csv",
    ];
    match runner.run("wmic", &args).await {
        Some((text, 0)) => parse_wmic_video(&text),
        _ => Vec::new(),
    }
}

#[cfg(target_os = "macos")]
async fn platform_gpus(runner: &dyn ProbeRunner) -> Vec<Gpu> {
    let mut gpus = match runner.run("system_profiler", &["SPDisplaysDataType"]).await {
        Some((text, 0)) => parse_system_profiler_displays(&text),
        _ => Vec::new(),
    };
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

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
async fn platform_gpus(_runner: &dyn ProbeRunner) -> Vec<Gpu> {
    Vec::new()
}

/// Parse `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits` output: one
/// `name, MiB` line per GPU. A line without a name is skipped; an unreadable memory value leaves
/// `vram_gb` unset.
pub fn parse_nvidia_smi_csv(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split(',').map(str::trim);
            let name = parts.next().filter(|name| !name.is_empty())?;
            Some(Gpu {
                name: name.to_string(),
                vram_gb: parts.next().and_then(parse_memory).map(round1),
                unified: false,
                backends: vec!["cuda".into(), "vulkan".into()],
            })
        })
        .collect()
}

/// Parse `wmic path win32_VideoController get Name,AdapterRAM /format:csv` output: `Node,AdapterRAM,
/// Name` rows, with the header skipped. `AdapterRAM` is a 32-bit WMI field, so a card with 4 GiB
/// or more reports at most 4 GiB here.
pub fn parse_wmic_video(text: &str) -> Vec<Gpu> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("Node,"))
        .filter_map(|line| {
            let mut columns = line.splitn(3, ',').map(str::trim);
            let _node = columns.next()?;
            let ram_bytes = columns.next().unwrap_or_default();
            let name = columns.next().filter(|name| !name.is_empty())?;
            let vram_gb = ram_bytes
                .parse::<u64>()
                .ok()
                .filter(|bytes| *bytes > 0)
                .map(|bytes| round1(bytes as f64 / GIB));
            Some(Gpu {
                name: name.to_string(),
                vram_gb,
                unified: false,
                backends: backends_for(name),
            })
        })
        .collect()
}

/// Parse the text form of `system_profiler SPDisplaysDataType`: a `Chipset Model:` line starts
/// each GPU and a `VRAM...: 8 GB` line gives its memory. An Apple GPU is reported as unified
/// memory.
pub fn parse_system_profiler_displays(text: &str) -> Vec<Gpu> {
    let mut gpus = Vec::new();
    let mut current: Option<Gpu> = None;
    for line in text.lines().map(str::trim) {
        if let Some(model) = line.strip_prefix("Chipset Model:") {
            gpus.extend(current.take());
            let name = model.trim();
            current = Some(Gpu {
                name: name.to_string(),
                vram_gb: None,
                unified: name.to_ascii_lowercase().contains("apple"),
                backends: vec!["metal".into()],
            });
        } else if let Some(rest) = line.strip_prefix("VRAM") {
            if let (Some(gpu), Some((_, raw))) = (current.as_mut(), rest.split_once(':')) {
                if gpu.vram_gb.is_none() {
                    gpu.vram_gb = parse_memory(raw).map(round1);
                }
            }
        }
    }
    gpus.extend(current);
    gpus
}

/// Parse a memory size into GB. Accepts `GB`/`GiB`, `MB`/`MiB` and `KB`/`KiB` suffixes in any
/// case. A bare number is MB, except above 16,000,000 where it is read as bytes. Zero, an empty
/// value and text without a number give `None`.
pub fn parse_memory(raw: &str) -> Option<f64> {
    let lower = raw.trim().to_ascii_lowercase();
    let number: f64 = lower
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .find(|part| !part.is_empty())?
        .parse()
        .ok()?;
    let gb = if lower.contains("gib") || lower.contains("gb") {
        number
    } else if lower.contains("mib") || lower.contains("mb") {
        number / 1024.0
    } else if lower.contains("kib") || lower.contains("kb") {
        number / 1024.0 / 1024.0
    } else if number > 16_000_000.0 {
        number / GIB
    } else {
        number / 1024.0
    };
    (gb > 0.0).then_some(gb)
}

fn backends_for(name: &str) -> Vec<String> {
    if name.to_ascii_lowercase().contains("nvidia") {
        vec!["cuda".into(), "vulkan".into()]
    } else {
        vec!["vulkan".into(), "directml".into()]
    }
}

/// Merge GPUs that report the same name (case-insensitively), keeping the first entry and filling
/// its missing VRAM from later ones.
fn dedupe_gpus(devices: Vec<Gpu>) -> Vec<Gpu> {
    let mut deduped: Vec<Gpu> = Vec::new();
    for device in devices {
        if device.name.trim().is_empty() {
            continue;
        }
        if let Some(existing) = deduped
            .iter_mut()
            .find(|existing| existing.name.eq_ignore_ascii_case(&device.name))
        {
            if existing.vram_gb.is_none() {
                existing.vram_gb = device.vram_gb;
            }
            continue;
        }
        deduped.push(device);
    }
    deduped
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    /// Drive a future that never waits. The fakes below resolve immediately.
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("a test probe future must resolve immediately"),
        }
    }

    /// Answers each program with a canned reply; an unknown program gives `None`.
    struct Fake(Vec<(&'static str, Option<(String, i32)>)>);

    impl ProbeRunner for Fake {
        fn run<'a>(&'a self, program: &'a str, _args: &'a [&'a str]) -> ProbeFuture<'a> {
            let answer = self
                .0
                .iter()
                .find(|(name, _)| *name == program)
                .and_then(|(_, reply)| reply.clone());
            Box::pin(async move { answer })
        }
    }

    fn reply(text: &str, status: i32) -> Option<(String, i32)> {
        Some((text.to_string(), status))
    }

    #[test]
    fn nvidia_smi_rows_parse_names_and_mib_and_skip_blank_names() {
        let gpus =
            parse_nvidia_smi_csv("NVIDIA GeForce RTX 4090, 24564\nTesla T4, [N/A]\n\n, 100\n");
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
        assert_eq!(gpus[0].vram_gb, Some(24.0));
        assert_eq!(gpus[0].backends, vec!["cuda", "vulkan"]);
        assert_eq!(gpus[1].vram_gb, None);
    }

    #[test]
    fn memory_strings_convert_to_gb_across_units_and_bare_numbers() {
        assert_eq!(parse_memory("8192"), Some(8.0));
        assert_eq!(parse_memory(" 8 GB"), Some(8.0));
        assert_eq!(parse_memory("16384 MiB"), Some(16.0));
        assert_eq!(parse_memory("1234567890"), Some(1_234_567_890.0 / GIB));
        assert!(parse_memory("2048 KB").unwrap() > 0.0);
        assert_eq!(parse_memory("0 MB"), None);
        assert_eq!(parse_memory(""), None);
        assert_eq!(parse_memory("[N/A]"), None);
    }

    #[test]
    fn wmic_rows_read_adapter_ram_in_bytes_and_keep_commas_in_names() {
        let gpus = parse_wmic_video(
            "Node,AdapterRAM,Name\r\nDESK,4293918720,NVIDIA GeForce RTX 3070\r\n\
             DESK,,Microsoft Basic Display Adapter\r\nDESK,8589934592,Acme, Inc GPU\r\n",
        );
        assert_eq!(gpus.len(), 3);
        assert_eq!(gpus[0].vram_gb, Some(4.0));
        assert_eq!(gpus[0].backends, vec!["cuda", "vulkan"]);
        assert_eq!(gpus[1].vram_gb, None);
        assert_eq!(gpus[2].name, "Acme, Inc GPU");
        assert_eq!(gpus[2].vram_gb, Some(8.0));
    }

    #[test]
    fn system_profiler_text_splits_gpus_and_marks_apple_as_unified() {
        let gpus = parse_system_profiler_displays(
            "Graphics/Displays:\n\n    Apple M3 Pro:\n\n      Chipset Model: Apple M3 Pro\n      \
             Type: GPU\n\n    AMD Radeon RX 6600:\n\n      Chipset Model: AMD Radeon RX 6600\n      \
             VRAM (Total): 8 GB\n",
        );
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].name, "Apple M3 Pro");
        assert!(gpus[0].unified);
        assert_eq!(gpus[0].vram_gb, None);
        assert_eq!(gpus[1].name, "AMD Radeon RX 6600");
        assert!(!gpus[1].unified);
        assert_eq!(gpus[1].vram_gb, Some(8.0));
    }

    #[test]
    fn duplicate_gpu_names_merge_and_fill_missing_vram() {
        let merged = dedupe_gpus(vec![
            Gpu {
                name: "Card".into(),
                vram_gb: None,
                unified: false,
                backends: vec![],
            },
            Gpu {
                name: "card".into(),
                vram_gb: Some(6.0),
                unified: false,
                backends: vec![],
            },
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].vram_gb, Some(6.0));
    }

    #[test]
    fn probe_with_runner_takes_gpus_from_the_runner_and_never_from_the_host() {
        let runner = Fake(vec![(
            "nvidia-smi",
            reply("NVIDIA GeForce RTX 4090, 24564\n", 0),
        )]);
        let hw = block_on(Hardware::probe_with_runner(&runner));
        assert_eq!(hw.source, "detected");
        assert_eq!(hw.os, std::env::consts::OS);
        assert!(hw
            .gpus
            .iter()
            .any(|gpu| gpu.name == "NVIDIA GeForce RTX 4090"));
        assert!(!hw.profile.is_empty());

        let failing = Fake(vec![(
            "nvidia-smi",
            reply("NVIDIA GeForce RTX 4090, 24564\n", 9),
        )]);
        let hw = block_on(Hardware::probe_with_runner(&failing));
        assert!(!hw
            .gpus
            .iter()
            .any(|gpu| gpu.name == "NVIDIA GeForce RTX 4090"));
    }

    #[test]
    fn runtimes_are_detected_only_on_a_zero_exit_with_a_hint_each() {
        let runner = Fake(vec![
            ("ollama", reply("ollama version is 0.5.1\n", 0)),
            ("llama-server", None),
            ("vllm", reply("0.6.0", 1)),
        ]);
        let found = block_on(detect_inference_runtimes(&runner));
        let ids: Vec<&str> = found.iter().map(|runtime| runtime.id.as_str()).collect();
        assert_eq!(ids, ["ollama", "llama-cpp", "vllm"]);
        assert!(found[0].detected);
        assert_eq!(found[0].version.as_deref(), Some("ollama version is 0.5.1"));
        assert!(!found[1].detected && found[1].version.is_none());
        assert!(!found[2].detected);
        assert!(found.iter().all(|runtime| !runtime.install_hint.is_empty()));
    }
}
