# RK2 Spike B results: Windows.Graphics.Capture for a WebView2 HWND (2026-10-08)

Host: this Windows host (`D:\Claude`), Windows 11 Pro, build **26300** (`[Environment]::OSVersion` = 10.0.26300.0). Plan §4 names the XPS; this run is on the host available to the spike and applies to this build only.

## Verdict

- **HWND capture works for an off-screen-visible WebView2 window.** A `GraphicsCaptureItem` created with `IGraphicsCaptureItemInterop::CreateForWindow` delivered 115 frames at 26 fps while the window sat at (-10000,-10000), outside every monitor. Every frame read `STEP-07` and the counter advanced.
- **Occlusion does not leak.** With a topmost animating marker covering the target, 111 captured frames contain zero marker pixels and zero marker OCR tokens.
- **`IsBorderRequired = false` works** on build 26300 (set succeeded, readback `false`). Build 22000 or later is required.
- **Hidden and minimized windows are not capturable** with a live session: no frames arrive after the change. A fresh session on an SW_HIDE window is refused by `CreateForWindow`. Neither state yields frames, so neither is supported by HWND capture. Plan R1 requires these states to be reported as unsupported, not degraded.
- **Recommendation:** use HWND capture as the primary Windows backend for the off-screen-visible state. Declare app-hidden (SW_HIDE and WebView2 IsVisible=false) and minimized as unsupported for HWND capture. If app-hidden must be supported, the only fallback is an app-owned render surface, which this spike did not test. No monitor crop was used anywhere.

## Commands run

```
C:\Users\adrds\.dotnet\dotnet.exe add package Microsoft.Web.WebView2        # 1.0.4258.31
C:\Users\adrds\.dotnet\dotnet.exe add package Vortice.Direct3D11            # 3.8.3
C:\Users\adrds\.dotnet\dotnet.exe add package Vortice.DXGI                  # 3.8.3
C:\Users\adrds\.dotnet\dotnet.exe add package Microsoft.Windows.CsWinRT     # 2.3.1
C:\Users\adrds\.dotnet\dotnet.exe build -c Release                          # in tools\rightkit\spikes\rk2-windows
powershell -NoProfile -ExecutionPolicy Bypass -File run-spike.ps1           # full run, launched hidden in background, log redirected
powershell -NoProfile -ExecutionPolicy Bypass -File analyze-spike.ps1 -RunDir <run dir>
```

Final run directory: `scratchpad\rk2\run-20261008-224010` (scratchpad root `C:\Users\adrds\AppData\Local\Temp\claude\D--Claude\022ffa73-ccba-4bbd-9028-0cd5875863a9\scratchpad\rk2\`). Each state has `summary.json`, `frames.json`, `ocr.json` and one PNG per frame.

Setup detail: the installed SDK is 10.0.26100, not 10.0.22621, so the CsWinRT projection needed `<CsWinRTWindowsMetadata>10.0.26100.0</CsWinRTWindowsMetadata>`. The TFM stays `net10.0-windows10.0.22621.0`.

## Decision table (final run, 4 s visible, state change at 3000 ms, capture 8 s)

Segments: "before" is 0–2900 ms; "after" starts 700 ms after the change to allow settling.

| State (live: session running before the change) | Frames arrive? | Content correct? (step id / counter) | Black or stale? | Support |
|---|---|---|---|---|
| Visible baseline (no change) | Yes: 223 frames, 27.9 fps | 223/223 step; counter 223/223 read, 222/222 increasing pairs (144 → 619) | No black; 0 identical consecutive pairs | Supported |
| Occluded by topmost marker (marker appears at 3000 ms) | Yes: 92 frames before, 111 after, 25.5 fps after | 111/111 step; 111/111 counter, 110/110 increasing (1449 → 1926 in this run's OCR, see note) | No black; marker present in 0 frames (red fraction 0.0, OCR 0 frames) | Supported (marker excluded) |
| Off-screen (move to -10000,-10000 at 3000 ms) | Yes: 115 frames after, 26.3 fps | 115/115 step; 115/115 counter, 114/114 increasing | No black | Supported (window rect at end = -10000,-10000, visible=true) |
| App-hidden: `ShowWindow SW_HIDE` at 3000 ms | No: last frame at 2989 ms, 0 frames after | n/a | Stale: no new frames (last pre-hide frame remains) | Unsupported (live) |
| App-hidden, fresh session after SW_HIDE | `CreateForWindow` returns E_INVALIDARG (`The parameter is incorrect.`) | n/a | n/a | Unsupported (item refused) |
| App-hidden: WebView2 control `Visible=false` at 3000 ms | No: 0 frames after change (last at ~2971 ms) | n/a | Stale: no new frames | Unsupported (live) |
| Minimized (`SW_MINIMIZE` at 3000 ms) | No: 0 frames after change | n/a | Stale | Unsupported (live) |
| Minimized, fresh session | Item created, 0 frames | n/a | n/a | Unsupported |
| Minimize then restore (restore at 5500 ms) | No frames while minimized; frames resume ~1.0 s after restore (first at 6549 ms) | 30/30 step and counter after restore; 29/29 increasing; 141/142 step over the whole run | No black | Recovers after restore |

Notes on the table:

- The pixel leak metric is `redFraction` (R>180, G<90, B<90) sampled every 4th pixel. Visible baseline: 0. Occluded run: 0 across all 111 post-change frames. Positive control (capturing the marker window itself, J): 1.00.
- OCR leak tokens: occluded run 0 frames. Positive control J: `LEAK-MARKER-7F3A` read in 92 of 93 frames, so the OCR leak check is validated too.
- The B counter range in the OCR summary (1449 → 1926) is for the whole run, not just the after-change segment. The "after" segment itself was verified as 110/110 increasing.
- Frame timing: `measuredFps` comes from frame system-relative timestamps. Frames are delivered only when content changes, so the rate is limited by WebView2 repaint, not by the capture.

## OCR output (Windows.Media.Ocr, English (United States))

- Visible baseline, first frame: `RK2 target window STEP-07 FRAME 00144 x`
- Occluded, first frame: `RK2 target window STEP-07 FRAME 01449 x`
- Off-screen, first frame: `RK2 target window STEP-07 FRAME 02859 x` (the trailing `x` is the title-bar close glyph)
- Minimize then restore, first frame after restore: `RK2 target window STEP-07 FRAME 05933 x`; last frame: `FRAME 06259`
- Marker positive control: `LEAK-MARKER-7F3A 23`

Per-state OCR counters are in each `ocr.json`. Counter values in the summary are cumulative across the run.

## Build, access and border evidence

- Windows build: **26300**.
- `GraphicsCaptureAccess.RequestAccessAsync(Borderless)`: `Allowed` (no prompt).
- `session.IsBorderRequired = false`: set OK; readback `false`.
- `session.IsCursorCaptureEnabled = false`: set OK.
- Item size reported as 1964x1282 for the 800x520 window. The spike process is DPI-unaware, so this is physical pixels at about 245% scale. Window rects in the logs are logical (virtualised) coordinates.

## Sample frames (kept small, roughly 90–130 KB each)

Under `D:\Claude\tools\rightkit\spikes\rk2-windows\samples\`:

- `A-visible-baseline-last.png`
- `B-occluded-by-marker-last.png`
- `C-off-screen-last.png`
- `D-app-hidden-SW_HIDE-last.png` (last frame before the hide)
- `E-app-hidden-webview2-last.png` (last frame before the WebView2 hide)
- `F-minimized-last.png` (last frame before minimize)
- `G-minimize-then-restore-last.png`
- `J-control-marker-self-last.png` (positive control)

## Limitations and unverified items

- **No visual confirmation of overlap or hiding.** The Windows-MCP screenshot tool and GDI screen grabs in this session did not show the spike windows (black output, "no windows found"). Overlap, off-screen placement and the WebView2 hide are evidenced by window rects, visibility flags, the leak metrics and capture behaviour, not by a screenshot. Unverified visually.
- **WebView2 hide mechanism.** The WinForms `WebView2.Visible=false` was used because the WinForms control does not expose `CoreWebView2Controller`. It is assumed to forward to the controller's `IsVisible`. The effect (frames stop) is measured; the controller property was not read back.
- **Single run, single host, single build (26300).** Not repeated; not run on the XPS named in the plan.
- **Visible-restored grey frame.** An earlier run showed one grey frame after minimize and restore with no WebView2 content. The final run did not reproduce it: content returned about 1 s after restore. This was not investigated further.
- **Harness bugs found and fixed during the spike** (affect any future harness, not the capture itself):
  1. Launching the window-op child process with `-WindowStyle Hidden` made `ShowWindow(SW_SHOW)` ineffective. SW_RESTORE and a direct PowerShell `ShowWindow(5)` were unaffected. Fixed by not hiding the child. Root cause not established.
  2. A reused marker `control.txt` still contained `exit`, closing the next marker immediately. Fixed with per-run control and HWND files.
  3. The orchestrator's inline per-state summary printed wrong segment counts. The per-state numbers in this document come from `analyze-spike.ps1`, which reads the raw files.
  4. A handler race at shutdown (NullReferenceException after the last frame) was fixed with a `closing` flag under the frame lock.
- Vortice (D3D11 device/readback) and CsWinRT (WinRT marshalling) are NuGet dependencies added beyond the WebView2 and CsWinRT set named in the brief. `Microsoft.Windows.CsWinRT` is required for the WinRT marshalling types.

## Files

- Spike source: `D:\Claude\tools\rightkit\spikes\rk2-windows\` (`rk2.csproj`, `Program.cs`, `Native.cs`, `TargetApp.cs`, `Capturer.cs`, `OcrRunner.cs`, `WindowOps.cs`)
- Orchestrator and analysis: `run-spike.ps1`, `analyze-spike.ps1`
- Machine-readable results: `D:\Claude\tools\rightkit\spikes\rk2-windows\results.json`
- Samples: `D:\Claude\tools\rightkit\spikes\rk2-windows\samples\`
- This document: `D:\Claude\tools\rightkit\docs\plans\2026-10-08-rk2-spike-b-results.md`
