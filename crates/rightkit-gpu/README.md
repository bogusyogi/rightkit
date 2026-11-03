# rightkit-gpu

The shared wgpu device context for Right Suite apps: per-OS backend policy, a crash sentinel
around device creation, adapter pick and limits, an exit gate for in-flight GPU work, and a
device-memory budget that halves on out-of-memory. Native only (wgpu 30).

**Use it** for the device and its lifecycle in Ember and CutRight. **Do not use it** for kernels,
buffer pools or render paths: those stay in the app.

```rust
use rightkit_gpu::{backend, exit, ContextOptions, GpuContext};

backend::set_init_marker(Some(marker_path));
if let Some(backends) = backend::compute_backends("EMBER_GPU_BACKEND") {
    let options = ContextOptions::new(backends, "ember");
    let gpu = GpuContext::create(&options, |reason| { /* stop using the GPU */ })?;
    // build kernels under backend::with_marker("kernel compilation", || ...)
}
// at exit:
exit::begin_shutdown();
exit::wait_idle(std::time::Duration::from_secs(5));
```

Every entry into the device goes through `exit::enter()`. See `CHANGELOG.md` for the wgpu pin
decision and `NOTICE` for the upstream attribution.
