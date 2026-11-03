# Changelog

## 0.1.0 (unpublished)

- New crate (rightkit#1160). One owner for the platform memory calls the Ember, Pulse and `rightkit-fsindex` trees each carried as their own FFI copy:
  - `release_free_memory() -> usize`: macOS `malloc_zone_pressure_relief` over all zones, returns freed allocator pages; `0` elsewhere.
  - `total_physical_memory() -> Option<u64>`: Windows `GlobalMemoryStatusEx`; `None` elsewhere or on failure.
  - `window_monitor_profile() -> Option<PathBuf>`: Windows ICC profile of the monitor showing the calling thread's main window; `None` elsewhere, without a monitor, or on failure.
- `unsafe` is confined to the three FFI modules, each block with a `// SAFETY:` comment; the public API is safe and never panics.
- Donor: Ember `crates/sysmem` (`lightcraft-sysmem`). Its unit tests are not carried over (end-to-end tests only; no debug assertions).
