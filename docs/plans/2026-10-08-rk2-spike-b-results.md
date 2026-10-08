# RK2 Spike B results: Windows.Graphics.Capture for a WebView2 HWND (2026-10-08, with follow-up)

## Host

- Machine: `BOGUS-DELL`, Dell XPS 15 9530.
- Windows build: **10.0.26300.9550** (CurrentBuild 26300, UBR 9550, DisplayVersion 26H2). The registry ProductName still reads "Windows 10 Pro"; build 26300 is a Windows 11 build (22000 or later), so the registry label is stale.
- GPUs: NVIDIA GeForce RTX 4070 Laptop GPU (driver 32.0.16.1692); Intel(R) Iris(R) Xe Graphics (driver 32.0.101.7084). The Intel adapter drives the internal panel.
- Display: one display, `\\.\DISPLAY1`, primary. Logical 1382x864 at 250% scaling, physical 3456x2160.
- **Monitor refresh: 60 Hz** (Win32_VideoController on the Intel adapter: CurrentRefreshRate 60, MinRefreshRate 60, MaxRefreshRate 60).

Earlier revisions of this document named the host only as "this Windows host". This is the XPS, and the numbers below are for this build and GPU.

## Verdict

- **HWND capture works for an off-screen-visible WebView2 window.** A `GraphicsCaptureItem` for the HWND delivers frames with correct content while the window sits at (-10000,-10000). Unchanged from the first run.
- **Hidden and minimized windows yield no frames.** SW_HIDE, WebView2 `Visible=false` and minimize all stop delivery. A fresh session on an SW_HIDE window is refused by `CreateForWindow`. These states are unsupported for HWND capture. The only fallback in the plan is an app-owned render surface, which this spike has not tested.
- **60 fps is delivered by the capture path, but not by our full per-frame pipeline.** Without per-frame PNG encoding, arrivals are 59.9 fps with no readback and 60.0 fps with the GPU copy and CPU map. The fixture paints at 60.0 Hz. The shortfall in the full pipeline (about 24 fps) comes from PNG encoding inside the frame callback, not from readback. The 60 fps requirement holds only if the consumer keeps up.
- **30 fps is delivered when the content is 30 fps** (30.0 with no readback, 30.1 with map). The 30 configuration was a content-side rate, because a capture-side cap is not available (see Frame rate).
- **Desktop screenshot: not obtained.** Every desktop grab returned near-black frames (see Placement). Placement is evidenced by window API readings at the time of each capture, not by a screenshot.
- **windows-rs probe: not built.** The managed broker refused the build (see windows-rs). The probe source is written and unverified.
- **Recommendation:** HWND capture as the Windows backend for the off-screen-visible state. Declare SW_HIDE, WebView2-hidden and minimized as unsupported for HWND capture. Do not put PNG or any slow work in the frame callback: copy the texture, or hand it to a worker thread, and keep the pool drained.

## Frame rate

### Method

The fixture animates on `requestAnimationFrame`. A counter increments per rAF, a bar moves on every update, and the target logs the rAF count to `target\paint.log` once per second.

Two content configurations:

- **content60:** repaint on every rAF. Fixture rAF rate 60.0 Hz.
- **content30:** repaint on every second rAF. Fixture rAF rate still 60.0 Hz; content updates 30 Hz.

Capture-side rate control was not applied. `GraphicsCaptureSession.MinUpdateInterval` is absent from the Microsoft.Windows.SDK.NET.Ref metadata for 10.0.22000, 10.0.22621 and 10.0.26100, so the 30 configuration is content-side. Frame pool size was 2 in every run; no other pool sizes were tested.

Each run captures 8 s. Intervals are between arrival timestamps taken in the FrameArrived callback before any readback. Dropped-counter gaps come from the on-screen counter, read by OCR, and count fixture repaints that were not delivered. "Map" is the GPU copy and CPU map without PNG. "Full" is map plus PNG per frame. Source run: `fps-20261008-225503`.

### Per-run results

| Run | Readback | Content | Arrivals | Delivered fps | Mean ms | p50 ms | p95 ms | Max ms | Gaps >1.5x median | Fixture rAF Hz | Missed paints (OCR counter) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| S60-visible-none | none | 60 | 480 | 59.89 | 16.70 | 16.66 | 17.56 | 31.90 | 1 | 60.02 | 0 |
| S60-visible-map | map | 60 | 481 | 60.03 | 16.66 | 16.66 | 17.68 | 21.05 | 0 | 60.01 | 0 |
| R60-visible-full | full | 60 | 198 | 24.64 | 40.58 | 43.40 | 49.23 | 52.66 | 0 | 60.01 | 281 |
| R60-occluded-full | full | 60 | 194 | 24.17 | 41.38 | 44.19 | 50.25 | 54.86 | 0 | 60.06 | 284 |
| R60-offscreen-full | full | 60 | 199 | 24.75 | 40.41 | 42.09 | 50.70 | 54.38 | 0 | 60.00 | 280 |
| S30-visible-none | none | 30 | 240 | 29.94 | 33.40 | 33.33 | 48.67 | 60.46 | 4 | 60.09 | 0 |
| S30-visible-map | map | 30 | 242 | 30.13 | 33.19 | 33.34 | 48.47 | 50.33 | 1 | 60.11 | 0 |
| R30-visible-full | full | 30 | 142 | 17.63 | 56.72 | 37.30 | 99.38 | 120.52 | 64 | 60.00 | 97 |
| R30-occluded-full | full | 30 | 195 | 24.32 | 41.11 | 43.48 | 51.24 | 54.84 | 0 | 60.00 | 45 |
| R30-offscreen-full | full | 30 | 185 | 23.11 | 43.28 | 45.81 | 52.92 | 57.51 | 0 | 59.97 | 54 |

Notes:

- The `R30-visible-full` run has a stall: 64 gaps, p95 99 ms. It is the only run that shows one, and I did not repeat it. Every full-readback run also started a 2 s WGC monitor capture at the 3 s mark, which may have contributed. Treat the full-pipeline numbers as carrying that confound.
- "Fixture rAF Hz" is the rAF count per second from `paint.log`. In the content30 runs rAF still runs at 60 Hz; the content updates at 30 Hz.
- Missed paints in the 30 configuration are counted in 2-count steps (one content update = 2 rAF).

### Stage attribution (visible state)

| Stage | 60 configuration | 30 configuration |
|---|---|---|
| Fixture paint, rAF rate (`paint.log`) | 60.0 Hz | 60.0 Hz (content 30 Hz) |
| FrameArrived, no readback | 59.89 fps | 29.94 fps |
| FrameArrived with GPU copy and CPU map | 60.03 fps | 30.13 fps |
| Full pipeline (map plus PNG per frame) | 24.6 fps | 17.6 to 24.3 fps |

The fixture paints at 60 Hz in every run. Arrivals match the paint rate with and without GPU readback. Dropping to about 24 fps happens only when each frame is also PNG-encoded in the callback (about 40 ms per frame). Dropped-counter gaps are zero in both stage runs and 280 to 284 per 8 s at 60 Hz in the full runs. The shortfall at 60 Hz is therefore the callback cost, not the fixture or the GPU path.

## Placement

- **Window placement at capture time** (`screen.json` per run, taken 3 s into each capture): fixture rect, IsWindowVisible, IsIconic and whether the rect intersects a monitor.
  - Visible: rect (100,100,800,520), visible true, not iconic, on monitor.
  - Occluded: same rect, visible true, not iconic, on monitor. A marker covered the fixture 80..940 x 80..640 for the whole capture; the marker HWND is recorded per run.
  - Off-screen: rect (-10000,-10000,800,520), visible true, not iconic, **not on any monitor**. Frames still show the step ID on every frame (185 and 199 frames).
- **Occlusion evidence from frames:** the fixture frames in all three occluded runs have zero red-marker pixels (maxRedFraction 0) and zero OCR marker-token frames (195 and 194 frames). Positive control, from the earlier run: the marker's own capture is 100% red, and OCR reads `LEAK-MARKER-7F3A` in 92 of 93 frames.
- **Placement evidence accepted:** the coordinator has accepted `screen.json` (GetWindowRect, IsWindowVisible, IsIconic, monitor intersection, taken during each capture) as placement evidence in place of a desktop screenshot. The desktop screenshot is not being retried.
- **Full-desktop screenshot: not obtained.**
  - GDI `CopyFromScreen` over the virtual screen: mean luminance 0 (about 0.97 in one run).
  - WGC `CreateForMonitor` on the primary monitor: near-black frames for the NVIDIA adapter (mean 0), the Intel adapter (0.48) and the Microsoft Basic Render adapter (0.51), out of 255.
  - Cause, as far as I can tell: another app's full-screen content in the console session (session 1, active) covers the desktop. The Windows-MCP screenshot from this session shows it. I did not inspect or keep that content, and I deleted every desktop grab from the run directories. Only the JSON placement records are kept.
  - To get the screenshot, close or minimize that full-screen app and re-run `run-fps.ps1`. I have not changed anything in the user's session.

## Build, access and border evidence

- Windows build 26300.9550 (above).
- `GraphicsCaptureAccess` Borderless: `Allowed`.
- `IsBorderRequired = false`: set OK, read back `false`. `IsCursorCaptureEnabled = false`: set OK.
- Item size reported at the fixture rect: 1964x1282 physical pixels for the 800x520 logical window (about 245% scaling; the spike process is DPI-unaware).

## windows-rs

- **Probe location:** `tools/rightkit/spikes/rk2-windows/rs-probe/` with its own `Cargo.toml` (`[workspace]` empty table), `src/main.rs`.
- **Crate and features:** `windows = "0.61"` (resolved to **0.61.3** in `rs-probe/Cargo.lock`) with features `Foundation`, `Foundation_Metadata`, `Security_Authorization_AppCapabilityAccess` (added for `GraphicsCaptureAccess::RequestAccessAsync`), `Graphics_Capture`, `Graphics_DirectX`, `Graphics_DirectX_Direct3D11`, `Win32_Foundation`, `Win32_Graphics_Direct3D`, `Win32_Graphics_Direct3D11`, `Win32_Graphics_Dxgi`, `Win32_Graphics_Dxgi_Common`, `Win32_System_WinRT`, `Win32_System_WinRT_Direct3D11`, `Win32_System_WinRT_Graphics_Capture`, `Win32_UI_WindowsAndMessaging`.
- **Intended behaviour (not executed):** `factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>` then `CreateForWindow`; `D3D11CreateDevice`; `CreateDirect3D11DeviceFromDXGIDevice`; `Direct3D11CaptureFramePool::CreateFreeThreaded`; `IsBorderRequired = false`; `IsCursorCaptureEnabled = false`; poll `TryGetNextFrame` for 10 frames and keep the last; `IDirect3DDxgiInterfaceAccess::GetInterface` for the texture; staging copy, `Map`, row-by-row copy; write a 32-bit top-down BMP and `probe.log`. It also checks `ApiInformation` for `GraphicsCaptureSession.MinUpdateInterval` at runtime.
- **Build attempt 1 (first run, `--manifest-path` from `D:\Claude`):** refused by the managed broker with `rightkit: RightKit broker endpoint is not configured`. Logged in `scratchpad\rk2\rs-build.log.err`.
- **Build, reported by the coordinator:** the probe builds when run from inside `rs-probe` without `--locked` on the first build (broker request `41e50ec5-31d0-4c20-b287-7aedf6f8ea9e`, Finished dev). The coordinator added `Security_Authorization_AppCapabilityAccess`. I did not observe this build; I have not verified it.
- **Run attempts (this session):** from `rs-probe`, `D:\.rightkit-managed\agent-bin\rightkit.cmd cargo run -- 24775762 <outdir>` against the visible fixture, run both through a hidden `cmd /c` child and directly from the shell. Both were refused with the same message, `rightkit: RightKit broker endpoint is not configured`, and no `RIGHTKIT`/`BROKER` environment variable is set in this session. I did not configure or repair the broker and did not use raw cargo. The off-screen run was not attempted, and no readback PNG/raw or log from the probe exists yet, so the step-ID OCR, the `IsBorderRequired`/`IsCursorCaptureEnabled` log check and the access-status check are all still outstanding.
- **Status:** the probe source is unverified by execution. Its API calls are listed in the results JSON under `windowsRsProbe`. Pending: a broker run against the visible and off-screen fixtures.
- **Gaps to check once the broker works:** whether `MinUpdateInterval` exists in windows 0.61 metadata (the C# SDK projection lacks it on three SDK versions); whether `IDirect3DDxgiInterfaceAccess::GetInterface` is generic in the crate; `D3D11CreateDevice` argument shapes; `Close()` on frame, session and pool.

## Earlier findings (first run, unchanged)

- Visible baseline: 223 frames at 27.9 fps (the first run's rate, before the rAF fixture change), step ID on every frame, counter increasing on 222 of 222 pairs.
- Occluded by marker (live): 111 frames after the change, step ID on every frame, marker absent.
- Off-screen (live): 115 frames after the change, step ID on every frame.
- SW_HIDE (live): frames stop at the hide; fresh capture refused (`E_INVALIDARG`).
- WebView2 `Visible=false` (live): no frames after the change.
- Minimized (live): no frames after the change. Fresh minimized session: zero frames.
- Minimize then restore: frames resume about 1 s after restore, with step ID and counter correct.
- Harness pitfalls fixed in the first run: `-WindowStyle Hidden` on the window-op child made `SW_SHOW` ineffective; a reused control file closed a new marker at once; the handler race at shutdown.

## Limitations and unverified items

- No desktop screenshot (see Placement). Placement rests on window API readings and the fixture's own frames.
- The full-pipeline numbers include a concurrent 2 s monitor capture at the 3 s mark, and one run (`R30-visible-full`) stalled. Stage runs (none, map) did not have the monitor capture.
- 30 fps is content-side. The capture-side `MinUpdateInterval` is not available in the SDK metadata here.
- Frame pool size 2 only.
- The WebView2 hide uses the WinForms control's `Visible`; the controller's `IsVisible` was not read back.
- One host, one build, one GPU configuration.

## Files

- Spike source: `D:\Claude\tools\rightkit\spikes\rk2-windows\` (`rk2.csproj`, `Program.cs`, `Native.cs`, `TargetApp.cs`, `Capturer.cs`, `Shot.cs`, `OcrRunner.cs`, `WindowOps.cs`).
- Scripts: `run-spike.ps1` and `analyze-spike.ps1` (first run); `run-fps.ps1` and `analyze-fps.ps1` (follow-up).
- Results: `D:\Claude\tools\rightkit\spikes\rk2-windows\results.json` (spike rows, host, frame rate, placement, windows-rs status) and `fps-results.json` (follow-up per-run table).
- Samples: `D:\Claude\tools\rightkit\spikes\rk2-windows\samples\` (first-run frames only; no desktop images).
- windows-rs probe: `D:\Claude\tools\rightkit\spikes\rk2-windows\rs-probe\` (not built).
- This document: `D:\Claude\tools\rightkit\docs\plans\2026-10-08-rk2-spike-b-results.md`.


## windows-rs build status (lead, 2026-10-08)

The probe compiles through the managed broker from inside  (request 41e50ec5-31d0-4c20-b287-7aedf6f8ea9e) on  0.61.3 with the listed features, including , which  requires. So the full API path (CreateForWindow, CreateDirect3D11DeviceFromDXGIDevice, CreateFreeThreaded, border/cursor flags, staging readback) type-checks in windows-rs. Not yet executed against a live fixture: subagents lack the broker endpoint, and the lead's runner script was not permitted in this session. Runtime readback remains open for RK3.
