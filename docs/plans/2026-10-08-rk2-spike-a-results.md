# RK2 Spike A results: macOS off-screen window capture (2026-10-08)

Host: Apple M6, macOS 27.0.1, one 1600x900 non-retina display (backing scale 1). Spike code: `tools/rightkit/spikes/rk2-macos/`. Evidence: `tools/rightkit/spikes/rk2-macos/evidence/<run>/` (14 MB, untracked, nothing committed or published).

## Verdict

**Spike A is PARTIAL, not green.** The capture, clock-mapping, input-telemetry, OCR and overlap (R12) halves work. One defect blocks the "counter advances in at least 95% of intervals for 60 s" criterion: the WKWebView in an off-screen window stops painting at 60 Hz after 17 to 47 s and falls to about 10 Hz. Its cause is unidentified and three mitigations did not remove it. Backend choice is decided (below); the rAF throttle must be root-caused before RK3 closes R1.

## Decision table (macOS)

| Visibility state | Verdict | Evidence |
|---|---|---|
| Off-screen-visible (window fully outside the union of all displays), occlusion SPI off, constrain override on | **Supported with an open defect** (rAF throttle after about 26 s). Needs two fixture-side measures, see "Required measures". | `offscreen-30fps-stream`, `offscreen-60fps-stream` (60 s each) |
| Occluded (another animating window above it, both off-screen) | **Supported, same rAF defect.** R12 pass: 0 foreign pixels. | `occluded-30fps-stream` |
| App-hidden (`NSApp hide:`) | **Unsupported.** | `apphidden-30fps-stream` |
| Minimized (`miniaturize:`) | **Unsupported.** | `minimized-30fps-stream` |
| Off-screen without the occlusion SPI (control) | **Unsupported.** | `offscreen-30fps-stream-nospi`, `stills/nospi-t10.png` |
| Off-screen via `SCRecordingOutput` direct-to-file (control) | **Unsuitable** (no PTS callbacks, duplicate frames). | `offscreen-30fps-file` |

### Metrics per run (60 s unless noted)

| Run | Frames delivered | Black frames | OCR step id correct (stable samples) | Counter advanced, page painting at 60 Hz | Counter advanced, page throttled | First throttled (s into capture) |
|---|---|---|---|---|---|---|
| offscreen 30 fps | 1673 (27.9/s) | 0 | 254/254 | 369/369 = 100% | 219/644 = 34% | 26.1 |
| offscreen 60 fps | 3447 (57.4/s) | 0 | 447/447 | 879/881 = 99.8% | 228/1240 = 18% | 26.1 |
| offscreen 30 fps, no inputs (45 s) | 1325 | 0 | 89/89 | 447/447 = 100% | 114/362 = 31% | 26.0 |
| occluded 30 fps | about 1768 | 0 | 277/277 | 306/306 = 100% | 267/781 = 34% | 17.4 |
| app-hidden 30 fps | 0 (1 callback, status `suspended`) | n/a, empty MP4 | none | none | none | n/a |
| minimized 30 fps | 0 (1 callback, status `suspended`) | n/a, empty MP4 | none | none | none | n/a |
| off-screen, no SPI, 20 s | 589 `complete` frames but window body is flat dark grey (`stills/nospi-t10.png`) | 0 by luma rule (grey 31), but content is not painted | 0/38 | strip never valid | n/a | page `visibilityState` = hidden, rAF 0 |
| off-screen direct-to-file 15 s | 443 (29.5/s), no per-frame callback | 0 | 13/30 | 144/281 = 51% (duplicates) | n/a | n/a |

Whole-run counter advance (the literal R1 metric) is 52 to 58% on all stream runs because of the throttle; only the pre-throttle segment meets 95%. Per-frame values are in each run's `analysis.jsonl`, summaries in `metrics.json`.

### Why each state

- **Off-screen-visible.** `SCContentFilter(desktopIndependentWindow:)` with `onScreenWindowsOnly:false` finds the window (`isOnScreen:true`, frame x=3600, display union is 0..1600). The window sits 2000 points past the union and `NSWindow.screen` is nil. SCK delivers frames at the requested rate from the first accepted frame (0.8 to 7 ms after callback) and the cursor is not in the pixels (`showsCursor=false`; the cursor is never over the window).
- **Occluded.** Occluder (magenta, animating, floating level) verified above the target by the on-screen z-order list (index 4 vs 6, `run.json:zorder_target_and_occluder`) and verified rendering magenta on its own (2394 magenta samples in `occluder-sanity`). The target capture has 0 magenta samples in all 1768 frames. The test is off-screen only; an on-screen overlap was not staged because it would have shown windows on Adrian's display.
- **App-hidden / minimized.** SCK lists the window with `isOnScreen:false` and `startCapture` succeeds, but it emits a single `suspended` frame and then nothing; MP4 is 0 bytes. The page stays `visibilityState: hidden` with rAF at 0 and the counter frozen. Minimize additionally made the fixture the frontmost app for a few seconds before Claude regained it (`minimized-30fps-stream/summary.json`, first foreground event), so it fails the focus rule on its own.
- **No SPI.** AppKit reports `occlusionState` 8192 (not visible) for an off-screen window; WebKit then sets the page hidden and stops rAF; the frame buffer shows an unpainted dark body.

## Required measures (macOS `launch_offscreen`)

1. **Defeat AppKit frame constraint.** `orderFront*` on a titled window pulls it back onto a display (observed at x=800 and x=1560, a visible window on Adrian's screen for seconds during early probing). A per-window identity override of `constrainFrameRect:toScreen:` fixes it. It must be installed on the window's class (`class_replaceMethod`, scoped to the Tao window class) and set only while the frame is set before ordering; a per-instance runtime subclass crashes Tao (next section), and installing the override before `setFrameOrigin:` zeroed the frame, so: set origin first, then install the override, then `orderFrontRegardless`. rightkit-control's existing scoped `UnconstrainedWindow` pattern alone is not enough because the window must stay unconstrained after ordering.
2. **Keep the page visible.** `-[WKWebView _setWindowOcclusionDetectionEnabled:NO]` (private SPI) applied before the window is first ordered gives `visibilityState: visible`, rAF 60 Hz, `occlusionState` still 8192. Applying it after ordering was ineffective in the one case tried (`vis_force` after `place`). `WKPreferences.inactiveSchedulingPolicy = .none` alone did nothing. This is private API: release and notarization review needs a decision, and a WebView2-style public alternative was not found.

## Open defect: rAF throttle after 17 to 47 s

In the fixture window the page's own rAF counter drops from 60 to 8 to 12 Hz at a seemingly random time; `visibilityState` stays `visible` and `occlusionState` stays 8192. Observed in 10 of 14 observation runs (4 recorded runs and 6 `probe_drop` variants dropped; 3 `probe_throttle` runs and 1 `probe_drop` run that began polling at 23 s did not). Not fixed by an `NSProcessInfo` latency-critical activity, by `inactiveSchedulingPolicy = .none`, or by removing capture (it happens with no capture and with eval-only polling). The quiet runs had no RPC polling at all (3 runs, 56 s) or began polling at 23 s (1 run, watched to 63 s); the pattern is not explained by time since load or since first call. Evidence: `evidence/throttle-probe.txt`, per-run `page_raf_rate_samples` in `metrics.json`, `probe_drop.py`, `probe_throttle.py`.
Untested next steps: a WebKit SPI that forces the web process foreground assertion (`_setVisibleOverride`-style), `NSWindowOcclusionState` forcing through a one-pixel on-display stub (rejected: R1 forbids visible pixels), or the app-owned render-surface fallback (the page renders to an offscreen surface the helper reads), which also sidesteps the SPI. Re-run this spike's `run.py offscreen 60 60` against any candidate.

## R2: telemetry and clock mapping (works)

- **Host clock to sample PTS.** SCStream sample PTS is the host clock; `CLOCK_UPTIME_RAW` ns and `CMClockGetTime(host)` agree to 0.000 to 0.001 ms (anchors in `summary.json:clock_anchors`, checked every second). Frame callback arrival minus PTS is 0.2 to 6.5 ms (30 fps run); one 60 fps run had a 279 ms outlier. Encoded PTS = host PTS minus first-accepted-frame PTS (rebased with `CMSampleBufferCreateCopyWithNewTiming`, `first_accepted_frame` event printed). Page epoch ms to uptime ns drifts under 1.4 ms over 60 s.
- **Direct-to-file cannot do this.** `SCRecordingOutput` exposes only started, finished and failed; no per-frame callback, no first-frame marker. It also produced duplicate frames (51% counter advance at 30 fps).
- **Admission, dispatch, completion.** `gate-journal.jsonl` (fixture gate hook = admission, `EffectStarted` = dispatch boundary, `EffectStopped` = completion) in `CLOCK_UPTIME_RAW` ns, joined to the caller send/receive timestamps in `run.json:inputs` and to encoded PTS in `metrics.json:inputs`. Admission lands 2.6 to 13 ms after caller send, dispatch 0.05 to 0.1 ms after admission. Completion: click 83 to 160 ms (rightkit-control pauses 50+40+40 ms inside a click), type 92 to 141 ms, wheel 6 to 11 ms, key 83 to 129 ms, drag 1.6 to 1.8 s. All inputs delivered with `isTrusted` true and `ok:true` while the window was off-screen, not key, in a non-active app (`page_log` entries `click:next`, `focus:input`, `input`, `pointerdown/up:scrub`, `scroll`, `key`). The "dispatch" timestamp is the gate boundary, not the native `NSEvent` post; native per-sub-event stamps would need hooks inside `mac_view.rs`.
- **Observed postcondition.** Page logs `postcondition:step` after two rAFs; OCR first sees the new step id 0 to 24 ms after it when the page is at 60 Hz. When throttled, the paint appears before the log (negative latencies, up to 90 ms, and one 751 ms outlier), so postcondition binding by page log lags the video under throttle.

## Foreground and focus (item 6)

The fixture was never the frontmost app in any run except the minimized one. In every run the frontmost app before and after the recording was unchanged at run level, with transitions among Safari, Claude and Codex during several runs that came from Adrian's own use (frontmost PIDs 78290, 698, 696, never the fixture; fixture PIDs in `run.json:info_initial.pid`). `lsappinfo front` plus a 50 ms in-helper monitor of the frontmost app and topmost non-fixture window.

## Permissions (item 7, R13)

Screen-recording permission was already effective: `CGPreflightScreenCaptureAccess()` returned true and SCK enumerated windows. The helper is a bare ad-hoc-signed CLI (`helper/rk2-capture`), so the grant is inherited from the responsible process, which is `/Applications/Claude.app` (`com.anthropic.claudefordesktop`, via its `disclaimer` helper). The helper has no identity of its own. Production `rightkit-record` needs its own signed helper identity, in its own TCC grant. The R13 negative test (second ad-hoc identity getting `permission_denied` and an empty bundle directory) was not run: the helper does fail closed (`requireScreenAccess()` exits 3 with `permission_denied` before writing anything) but a differently-attributed identity was not staged.

## OCR

`rightkit-ocr` was not used: its ONNX path returns `UnresolvedProvenance` on load and its macOS backend is the same Apple Vision API the helper calls directly (`VNRecognizeTextRequest`, accurate, region of interest). OCR recovered `STEP-nnn` in every stable sample of the supported runs (254/254, 447/447, 89/89, 277/277). The frame counter was decoded from a 24-cell bit strip drawn by the page (every frame valid) and cross-checked by OCR of the text.

## Crash and cleanup (coordinator note 2026-10-08)

The "quit unexpectedly" dialog came from the fixture. **Cause:** in `place`, the first attempt installed a persistent per-instance runtime subclass of the Tao `NSWindow` to bypass frame constraint. Tao's `send_event` then walks `superclass` of the window class and dereferences a null (`-[NSObject superclass]`, `KERN_INVALID_ADDRESS 0x8`, SIGSEGV on the main thread). rightkit-control's own version restores the class immediately for this reason. **Fix:** class-scoped `class_replaceMethod` on the window's class (no subclass), set after origin is placed. No fixture crash in any later run (about 25 launches). Reports for this cause: `~/Library/Logs/DiagnosticReports/rk2-fixture-2026-10-08-222941.ips`, `-222956.ips`, `-223431.ips`, `-223438.ips`, `-223449.ips`, `-223511.ips`.
A second, separate crash was in my Swift helper: `JSONSerialization` raised an Objective-C exception on `Double.infinity` (summary of a run with no complete frames), abort (SIGABRT). **Fix:** `isValidJSONObject` guard and a finite sentinel. Reports: `rk2-capture-2026-10-08-224725.ips`, `-230522.ips`, `-230628.ips`.
Containment now in both binaries: fatal signals exit silently (fixture: SEGV, BUS, ABRT, ILL, FPE; helper: SEGV, BUS, ABRT, ILL, TRAP) via `_exit`, so ReportCrash is not invoked and no dialog is raised; no global CrashReporter setting was changed. No fixture or helper process is left running.
Also noted: copies of the fixture launched as an `.app` through `open -g -n` stopped creating their configured window after the first success (`window_not_found` for 12 s+), while the bare binary always did; the harness therefore direct-execs the binary. Not root-caused; the five 22:29 to 22:35 crashes above may be related because the same binary was in use.

## Backend recommendation

- **Use `SCStream` with an `SCStreamOutput` frame callback and an `AVAssetWriter` MP4 writer, window-scoped via `SCContentFilter(desktopIndependentWindow:)` with `onScreenWindowsOnly:false`, selected by (pid, window id). Do not use `SCRecordingOutput` for the recorder.** It is the only path that supplies sample PTS, first-accepted-frame, frame status (`complete`/`idle`/`suspended`) and the clock anchors R2 needs.
- **rightkit-control gains `launch_offscreen`** and the macOS visibility state to qualify is **off-screen-visible only**. App-hidden and minimized are unsupported and must be reported as such, never degraded to something else. Occluded works because it is a subset of off-screen-visible.
- `launch_offscreen` must (a) place the window with the class-scoped constrain override, (b) read back that the frame is disjoint from all `NSScreen` frames and `NSWindow.screen == nil`, and (c) disable WKWebView occlusion detection before first order. (c) is private SPI; decide that, or the app-owned render-surface alternative, before RK3.
- Do not mark R1 green on macOS until the rAF throttle is understood or avoided, and until a 60 s run passes the 95% rule end to end.

## Reproduce

```
cd tools/rightkit
rightkit cargo build --manifest-path spikes/rk2-macos/fixture-app/Cargo.toml
rightkit cargo build --manifest-path spikes/rk2-macos/svc-call/Cargo.toml
cd spikes/rk2-macos/helper && swiftc -O main.swift -o rk2-capture && codesign -s - --force rk2-capture
cd .. && ./run-all.sh        # python3 run.py <offscreen|occluded|apphidden|minimized> <fps> <seconds> [stream|file]; analyze.py per run
```
(`tools/rightkit` workspace `cargo` is currently broken by the empty `crates/rightkit-align` directory, unrelated to this spike; both spike crates are standalone workspaces.)
Fixture: `spikes/rk2-macos/fixture-app` (Tauri 2 + rightkit-control `tauri-plugin`, step id, 24-bit frame-counter strip, 7 s animated and 3 s static per 10 s, page input log). Helper: `spikes/rk2-macos/helper/main.swift` (`list`, `zorder`, `record`, `analyze`).
