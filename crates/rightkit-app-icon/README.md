# rightkit-app-icon

Icons of applications for list rows (history, recents), as PNG bytes or a `data:image/png;base64,...` URL. "Not found" is `None`, never an error.

## Behavior
- Windows: large shell icon of a concrete executable via GDI, PNG-encoded in-process. Without a path: running processes by image name, `App Paths` registry, Start-menu `.lnk` targets (at most 4000 scanned), then packaged (MSIX/AppX) manifest logos. Executables under `\WindowsApps\` prefer the manifest logo. Logo variant is the smallest at least 48 px, unplated over plated.
- macOS: `NSWorkspace iconForFile` on the bundle path, or the running app whose localized name or bundle id matches; the largest representation that states a size within the limits is drawn into a bitmap this crate allocates (at most 512 x 512 x 4 bytes) and PNG-encoded by the crate's own encoder. AppKit is never asked for a TIFF or a PNG.
- Elsewhere: `None`.
- Cache: bounded in-memory map (PNG bytes; name-only misses expire after `name_only_miss_ttl`, path misses last the session) plus `<cache_dir>/<first 32 hex of sha256(key)>.png`, written atomically under the path key and the name key. Keys are `path:<path>` (lowercased on Windows) and `name:<lowercased name>`.
- A panic in a platform call is contained and reported through the event sink.
- Untrusted input is bounded (revisions 2 and 3): every read has a byte cap and oversize is "no icon", never a partial read; images are checked for dimensions and pixel count before anything is allocated; manifest logo paths are screened and opened only through `rightkit_fs::Roots` (no `..`, no absolute, drive or UNC path, no alternate data stream, no link or reparse point followed out of the package root); registry and directory enumeration has an entry, depth and time budget; a cached PNG is validated end to end, compressed data included, and a failing file is a miss that is rewritten. Manifest, logo and cache files are read with `require_single_link(true)`: a hard-linked file is "no icon".

## Limits
| Input | Limit | Over the limit |
| --- | --- | --- |
| `AppxManifest.xml` | 512 KiB read, UTF-8 only, 64 applications, logo value 260 bytes | no icon from that package |
| Logo PNG file | 2 MiB | next logo, else no icon |
| Cached PNG file | 2 MiB; opened through `Roots` (no link, single hard link) | miss, re-extracted and rewritten |
| Manifest and logo files | opened through `Roots` with no link followed and a single hard link (same policy as the cache); the package root is not treated as trusted | no icon from that package or logo |
| Any PNG (logo, cache, extracted) | full chunk walk with CRC, IHDR first, contiguous IDAT, IEND last, no trailing data; `max_edge` (clamped to 1024) and 262,144 pixels checked before use; IDAT concatenated and inflated with an output cap of exactly the decoded length the header implies (Adam7 included, at most 4 MiB), valid zlib and Adler-32, no bytes after the stream, filter bytes 0..=4 | rejected |
| Windows shell icon | `SM_CXICON`/`SM_CYICON` checked before the shell is asked; `BITMAP` header of colour and mask checked before `GetDIBits`; our buffer is at most 1 MiB; executable at most 1 GiB | no icon |
| macOS icon | file path at most 64 MiB (a bundle directory is accepted); rep sizes read from properties and checked; our own bitmap at most 512 x 512 x 4 bytes; no AppKit TIFF or PNG | no icon (the largest rep that fits is used) |
| Host strings (process name, executable path) | 64 KiB, no NUL | no icon |
| Start-menu `.lnk` | read by the crate with `open_confined`, 1 MiB, then loaded from memory | skipped |
| Logo qualifiers | `scale-N` 1..=1000, `targetsize-N` 1..=4096, checked arithmetic | that variant is skipped |
| Logo path | 260 bytes, 8 components, plain names only | that logo is skipped |
| Registry string | REG_SZ or REG_EXPAND_SZ, 64 KiB | no value |
| Package repository scan | 16,384 entries (keys, directory entries, manifest probes), 4 s | roots found so far are used |
| Start-menu walk | 20,000 entries of any kind, depth 8, 5 s, 4,000 shortcuts of at most 1 MiB; reparse points skipped | no shortcut match |
| Manifest probe for one `WindowsApps` exe | 2,048 entries, 2 s; logo directory 1,024 entries | no logo |
| Running-process lookup | 4,096 process ids, 2 s | no match |

Rejections are reported as `icon_rejected` with a detail of `<place>:<reason code>` (for example `manifest:oversize`, `cache:png_crc`, `logo:link_or_escape`); never a path or file contents.

## API
```rust
let icons = AppIcons::new(IconConfig { max_memory_entries: 512, ..IconConfig::new(Some(dir)) });
icons.data_url("Code.exe", Some(r"C:\...\Code.exe")); // Option<String>
icons.png("Code.exe", None);                           // Option<Vec<u8>>
```
Both calls block: run them on a blocking pool. Free functions: `cache_key`, `png_data_url`, `icon_cache_file_name`, `process_names_match`.

## Host inputs (`IconConfig`)
`cache_dir`, `name_only_miss_ttl`, `max_memory_entries`, `max_edge` (now applies on every platform and to cached PNGs, clamped to 1..=1024), `process_path_for_pid` (Windows), `event_sink(kind, detail)` with kinds `extract_panic`, `disk_put_failed` and `icon_rejected`.

## Installed verification

Windows qualification covers packaged logos, Start-menu shortcuts, cached icons & recovery from damaged cache files. macOS qualification covers bundle lookup, accurate colors & bounded output dimensions. AppKit controls internal allocations during native icon lookup & drawing.
