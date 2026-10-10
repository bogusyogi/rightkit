# Changelog

## 0.2.5

- New default-on feature `native` gates the `native` module (native-library catalog). Default
  builds are unchanged; FFmpeg-only consumers can build with `default-features = false`.
- Internal: `stage_external_bin` writes through `rightkit_fs::atomic_write_with` (temp file,
  fsync, atomic rename, directory fsync) instead of a hand-rolled copy and rename. The mode is
  set to `0755` before the rename, and on Windows an existing destination is replaced atomically
  instead of being deleted first. Temp files are now named `.<name>.rktmp-*`.
