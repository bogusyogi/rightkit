# Changelog

## 0.1.1

- Publish the dependency requirements main already declares: `rightkit-fs` 0.5.0, where the published 0.1.0 requires 0.4.15. The 0.1.0 manifest on crates.io stays as published; this version carries the new requirements.

## 0.1.0 (unpublished; revision 3 after the RightKit owner's hold on revision 2)
- PNG validation now covers the compressed data: the IDAT payloads are concatenated and inflated (`miniz_oxide`, new dependency, already in the RightKit lockfile) with an output buffer of exactly the decoded length the header implies plus one byte; the decoded length must match exactly (Adam7 handled), the zlib header, deflate data and Adler-32 must be valid, nothing may follow the stream, filter bytes must be 0..=4. Applies to logos, cache files and the crate's own and the platform's output.
- macOS: AppKit is no longer asked for a TIFF or a PNG. Representation sizes are read as properties and checked first; the crate allocates the only bitmap (at most 512 x 512), draws into it, and encodes with its own encoder. File paths over 64 MiB are refused before `iconForFile`.
- Windows: the large-icon metric is checked before the shell is asked; the colour and mask bitmaps are size-checked before any pixel buffer; the executable size is capped; Start-menu shortcuts are read by the crate (`open_confined`, 1 MiB) and loaded from memory.
- Manifest and logo reads use `require_single_link(true)`, as the cache already did. No trusted-root exemption.
- Host strings (process name, executable path) are capped at 64 KiB.
- `IconConfig`, cache keys and file names unchanged. New rejection codes `png_inflate` and `png_length`.

## 0.1.0 (unpublished; revision 2 after the RightKit owner's review)
- Bounded untrusted input: per-kind byte caps (manifest 512 KiB, logo 2 MiB, cached PNG 2 MiB, registry string 64 KiB, `.lnk` 1 MiB); oversize is "no icon", never a partial read.
- Qualifier and scale arithmetic is checked; out-of-range or overflowing `scale-N` / `targetsize-N` values reject that candidate; variants above `max_edge` are skipped.
- Manifest logo paths are screened and read only through `rightkit_fs::Roots` (`resolve_confined`, `open_confined` with `ConfinedOptions::read_only()`); no `..`, absolute, drive, UNC, alternate-data-stream path, link or reparse point leaves the package root.
- Images are bounded before allocation on Windows and macOS; every PNG (logo, cache, extracted) gets full structure, CRC and dimension validation (`max_edge` clamped to 1024, 262,144 pixels). `max_edge` now also applies on macOS and to cached PNGs.
- Cache reads open through `Roots` (no link, single hard link), are capped and validated; a failing file is a miss and is rewritten. File naming and keys are unchanged.
- Registry, Start-menu and package enumeration have entry, depth and time budgets; reparse points are neither followed nor visited; a registry-supplied package root must be a local drive-letter path.
- New event-sink kind `icon_rejected` with detail `<place>:<reason code>`. `IconConfig` fields are unchanged.
- The `png` module is compiled on every platform (CRC-32 for validation).

## 0.1.0 (revision 1)
- New crate, moved from HeardRight `src/app_icon.rs`: icon lookup by executable path, process name or bundle (Windows shell icon, running processes, `App Paths`, Start-menu shortcuts, packaged-app manifest logos; macOS `NSWorkspace`), dependency-free PNG encoder, persistent PNG cache keyed by SHA-256 of the lookup key.
- Host inputs: cache directory, miss TTL, memory-cache bound, maximum edge, process-path resolver, event sink.
- The in-memory cache is now bounded (`max_memory_entries`, cleared when full) and stores PNG bytes.

## Revision 3.1

- `limits.rs`: the zlib header is checked on the concatenated IDAT stream, not on the first chunk, so a valid empty or one-byte first IDAT is accepted (W3C PNG 11.2.3, 12.8). Chunk ordering and palette checks unchanged.
- `mac.rs`: the result of `NSImageRep.drawInRect` is honoured; a failed draw returns no icon and nothing is encoded or cached. The graphics state is restored on every path.
