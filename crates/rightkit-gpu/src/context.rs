//! The wgpu device context: instance, adapter pick, limits, device and queue, with the failure
//! callbacks wired. Adapted from Ember `crates/gpu/src/ctx.rs` (the generic part; Ember's kernels,
//! buffer pool and render paths stay in Ember).
//!
//! [`GpuContext::create`] enters the exit gate ([`crate::exit`]), writes the crash sentinel
//! ([`crate::backend`]) around driver work, catches a panic, and refuses a software adapter and an
//! adapter below the caller's storage-buffer minimum, each with a reason the app can show. An
//! uncaptured wgpu error or a lost device is logged and reported once through the `on_failure`
//! callback so the app can stop using the GPU.

use std::sync::Arc;

use crate::backend;
use crate::exit;

/// How the app wants the device made.
#[derive(Debug, Clone)]
pub struct ContextOptions {
    /// The backends to load (see [`crate::backend::compute_backends`]).
    pub backends: wgpu::Backends,
    /// Prefer the adapter called this when present (the app's setting); else the default choice.
    pub adapter_name: Option<String>,
    /// Ask wgpu for the low-power adapter instead of the high-performance one.
    pub low_power: bool,
    /// The fewest storage buffers per shader stage the app's kernels bind; fewer is refused.
    pub min_storage_buffers_per_stage: u32,
    /// Lower the device limits, to reproduce a smaller adapter ([`parse_limits_cap`]).
    pub limits_cap: Option<LimitsCap>,
    /// The device's debug label.
    pub label: String,
}

impl ContextOptions {
    /// The default options for `backends`: default adapter, high performance, no limit cap.
    pub fn new(backends: wgpu::Backends, label: impl Into<String>) -> Self {
        Self {
            backends,
            adapter_name: None,
            low_power: false,
            min_storage_buffers_per_stage: 8,
            limits_cap: None,
            label: label.into(),
        }
    }
}

/// An upper bound on the device's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitsCap {
    /// Most bytes in one storage binding.
    pub storage_binding_bytes: u64,
    /// Most bytes in one buffer.
    pub buffer_bytes: u64,
}

/// Parses a limits cap the way Ember's `LIGHTCRAFT_GPU_LIMITS` does: `webgpu` / `downlevel` /
/// `default` = the WebGPU defaults (128 MiB storage bindings, 256 MiB buffers), `<n>` (or `<n>MiB`,
/// `<n>MB`) = n MiB storage bindings and 2n MiB buffers. Empty or unparsable: `None`.
pub fn parse_limits_cap(text: &str) -> Option<LimitsCap> {
    let v = text.trim().to_ascii_lowercase();
    match v.as_str() {
        "" => None,
        "webgpu" | "downlevel" | "default" => {
            let d = wgpu::Limits::default();
            Some(LimitsCap {
                storage_binding_bytes: d.max_storage_buffer_binding_size,
                buffer_bytes: d.max_buffer_size,
            })
        }
        n => {
            let mb: u64 = n
                .trim_end_matches("mib")
                .trim_end_matches("mb")
                .trim()
                .parse()
                .ok()?;
            let bytes = mb.max(1) << 20;
            Some(LimitsCap {
                storage_binding_bytes: bytes,
                buffer_bytes: 2 * bytes,
            })
        }
    }
}

/// The index of the adapter called `wanted` in `names` (the first with that name), or `None` when
/// there is none, so the default choice is used.
pub fn pick_adapter(names: &[String], wanted: &str) -> Option<usize> {
    names.iter().position(|n| n == wanted)
}

/// A wgpu device and queue on a chosen adapter.
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    /// The largest buffer the device allows (the smaller of the storage binding and buffer limits).
    pub max_buffer: u64,
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("adapter", &self.info.name)
            .field("backend", &self.info.backend)
            .field("max_buffer", &self.max_buffer)
            .finish()
    }
}

/// The wgpu instance for `backends`: only these drivers are loaded.
fn new_instance(backends: wgpu::Backends) -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    // DX12 shaders compile with FXC (issue #471).
    desc.backend_options = backend::backend_options();
    // No indirect-call validation pipeline: Intel Metal fails to compile it (#250).
    desc.flags = backend::instance_flags_from_env(desc.flags);
    wgpu::Instance::new(desc)
}

/// The adapters of `instance` that can render (software ones left out), in the order wgpu lists them.
fn usable_adapters(instance: &wgpu::Instance, backends: wgpu::Backends) -> Vec<wgpu::Adapter> {
    pollster::block_on(instance.enumerate_adapters(backends))
        .into_iter()
        .filter(|a| a.get_info().device_type != wgpu::DeviceType::Cpu)
        .collect()
}

/// The names of the usable adapters for `backends`. Loads the drivers; the device is not created.
pub fn adapter_names(backends: wgpu::Backends) -> Vec<String> {
    usable_adapters(&new_instance(backends), backends)
        .iter()
        .map(|a| a.get_info().name)
        .collect()
}

impl GpuContext {
    /// Creates the device, or says why there is none (software adapter, too few storage buffers,
    /// no adapter, device creation failed, the process is ending, or a driver panic).
    ///
    /// `on_failure` is called with a reason, possibly from a driver thread, when wgpu reports an
    /// error outside any error scope or the device is lost; the app should stop using the GPU.
    pub fn create(
        options: &ContextOptions,
        on_failure: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<GpuContext, String> {
        // Device creation enters the driver and must not begin after process shutdown.
        let Some(_work) = exit::enter() else {
            return Err("the process is ending".into());
        };
        let on_failure: Arc<dyn Fn(String) + Send + Sync> = Arc::new(on_failure);
        backend::with_init_marker(options.backends, || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                create_unguarded(options, &on_failure)
            }))
            .unwrap_or_else(|_| Err("device creation panicked".into()))
        })
    }
}

fn create_unguarded(
    options: &ContextOptions,
    on_failure: &Arc<dyn Fn(String) + Send + Sync>,
) -> Result<GpuContext, String> {
    let backends = options.backends;
    let instance = new_instance(backends);
    let power_preference = if options.low_power {
        wgpu::PowerPreference::LowPower
    } else {
        wgpu::PowerPreference::HighPerformance
    };
    let by_name = options.adapter_name.as_deref().and_then(|wanted| {
        let found = usable_adapters(&instance, backends);
        let names: Vec<String> = found.iter().map(|a| a.get_info().name).collect();
        let index = pick_adapter(&names, wanted);
        if index.is_none() {
            log::warn!(
                "gpu: no adapter named {wanted:?} among {names:?}; using the default choice"
            );
        }
        index.and_then(|i| found.into_iter().nth(i))
    });
    let adapter = match by_name {
        Some(adapter) => adapter,
        None => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter among {backends:?} ({e})"))?,
    };
    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        log::info!("gpu: only a software adapter ({})", info.name);
        return Err(format!("software adapter ({}) skipped", info.name));
    }
    let mut limits = adapter.limits();
    if limits.max_storage_buffers_per_shader_stage < options.min_storage_buffers_per_stage {
        log::info!("gpu: {} has too few storage buffers per stage", info.name);
        return Err(format!(
            "{} has too few storage buffers per shader stage ({} < {})",
            info.name,
            limits.max_storage_buffers_per_shader_stage,
            options.min_storage_buffers_per_stage
        ));
    }
    if let Some(cap) = options.limits_cap {
        limits.max_storage_buffer_binding_size = limits
            .max_storage_buffer_binding_size
            .min(cap.storage_binding_bytes);
        limits.max_buffer_size = limits.max_buffer_size.min(cap.buffer_bytes);
        log::info!(
            "gpu: limits capped: storage bindings <= {} MiB, buffers <= {} MiB",
            cap.storage_binding_bytes >> 20,
            cap.buffer_bytes >> 20
        );
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some(options.label.as_str()),
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;

    // Errors outside a caller's error scopes: log, and tell the app to stop using the GPU.
    let uncaptured = Arc::clone(on_failure);
    device.on_uncaptured_error(Arc::new(move |e| {
        log::error!("gpu: {e}");
        uncaptured(format!("device error: {e}"));
    }));
    // wgpu reports a lost device (driver reset, GPU hang, watchdog) only here, never as an error.
    let lost = Arc::clone(on_failure);
    device.set_device_lost_callback(move |reason, msg| {
        log::error!("gpu: device lost ({reason:?}): {msg}");
        lost(format!("device lost ({reason:?}): {msg}"));
    });

    let max_buffer = limits
        .max_storage_buffer_binding_size
        .min(limits.max_buffer_size);
    log::info!(
        "gpu: {} ({:?}), buffers <= {} MiB",
        info.name,
        info.backend,
        max_buffer >> 20
    );
    Ok(GpuContext {
        device,
        queue,
        info,
        max_buffer,
    })
}
