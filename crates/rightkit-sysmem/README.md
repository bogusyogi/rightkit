# rightkit-sysmem

Safe API over the few platform memory calls the RightKit suite needs. The calls are FFI, so this is the one place they live; consumers never declare their own copy.

| Function | Platform | What it does |
| --- | --- | --- |
| `release_free_memory()` | macOS | Returns freed allocator pages to the system (`malloc_zone_pressure_relief`). Returns the bytes released. |
| `total_physical_memory()` | Windows | Physical RAM in bytes (`GlobalMemoryStatusEx`). |
| `window_monitor_profile()` | Windows | Path of the ICC profile assigned to the monitor of the calling thread's main window. |

Everywhere else each function returns nothing (`0`, `None`) and does no native call. Failures are reported the same way; nothing panics.

Dependencies: none. Licence: MIT OR Apache-2.0.
