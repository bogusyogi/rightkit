# Changelog

## 0.1.0 (unpublished)

- New crate (#1159): the shared GPU device context Ember and CutRight both carry. `backend` (per-OS backend policy, `compute_backends(app_var)`, `window_backends(app_var)`, DX12 compiler and instance-flag policy, the crash sentinel `set_init_marker` / `begin_startup_marker` / `take_init_marker`), `exit` (`enter`, `begin_shutdown`, `shutting_down`, `wait_idle`), `MemoryBudget` (fair admission, halves on out-of-memory, grows back one sixteenth per success), `GpuContext::create(ContextOptions, on_failure)`.
- Decision, wgpu pin: wgpu 30 only. Ember builds on 30 and `rightkit-compositor` already pins 30.0.1; the code ported here uses the 30 API (`InstanceDescriptor::new_without_display_handle`, async `enumerate_adapters`), which differs from 29 in exactly the calls this crate makes, so carrying both behind features would double the surface for the one consumer still on 29. CutRight moves to 30 (its `cutright-compositor-gpu` context is the 514-line twin and is replaced by this crate).
- Changes from Ember: the app's backend environment variable is a parameter (`EMBER_GPU_BACKEND` and the like) instead of `LIGHTCRAFT_GPU_BACKEND`; the process-wide caches and the egui window renderer choice stay in Ember; the device context carries no kernels, buffer pool or render paths. The unit tests of the Ember files were not carried over.
- Ember keeps its copy until it adopts this crate; the upstream fixes the files cite (#1764, #136, #620) are absorbed here from the move on.
- Not compiled or run when written. Needs a device run on Mac (Metal) and Windows (DX12): `GpuContext::create`, then drop the device with the sentinel armed, and exit through `exit::wait_idle`.
