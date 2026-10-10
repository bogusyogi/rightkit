# Changelog

## 0.2.1

- Dependencies: `chrono` removed (event timestamps are parsed by a small RFC 3339 parser) and
  `objc2-foundation` removed (macOS physical memory is read with `sysctl hw.memsize`; thermal
  state is read from the kernel's `com.apple.system.thermalpressurelevel` notify key, the source
  behind `NSProcessInfo.thermalState`, which has no sysctl key).
- Internal: crash-safe file replacement now uses `rightkit_fs::atomic_write_with` (only the
  `fs` feature: libc and thiserror, no network dependency). The `http` feature stays the only
  network dependency.
