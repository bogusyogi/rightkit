# RK2 Spike A results: macOS off-screen window capture (2026-10-08)

Host: Apple M6, macOS 27.0.1, one 1600x900 non-retina display (backing scale 1). Spike code: `rightkit/spikes/rk2-macos/`. Evidence: `rightkit/spikes/rk2-macos/evidence/<run>/` (14 MB, untracked, nothing committed or published).

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
cd rightkit
rightkit cargo build --manifest-path spikes/rk2-macos/fixture-app/Cargo.toml
rightkit cargo build --manifest-path spikes/rk2-macos/svc-call/Cargo.toml
cd spikes/rk2-macos/helper && swiftc -O main.swift -o rk2-capture && codesign -s - --force rk2-capture
cd .. && ./run-all.sh        # python3 run.py <offscreen|occluded|apphidden|minimized> <fps> <seconds> [stream|file]; analyze.py per run
```
(`rightkit` workspace `cargo` is currently broken by the empty `crates/rightkit-align` directory, unrelated to this spike; both spike crates are standalone workspaces.)
Fixture: `spikes/rk2-macos/fixture-app` (Tauri 2 + rightkit-control `tauri-plugin`, step id, 24-bit frame-counter strip, 7 s animated and 3 s static per 10 s, page input log). Helper: `spikes/rk2-macos/helper/main.swift` (`list`, `zorder`, `record`, `analyze`).

## rAF investigation (2026-10-09): root cause found, fixed with a public API

**Root cause: App Nap throttles the WebKit CVDisplayLink.** The fixture is a non-frontmost app with no window that AppKit considers visible (occlusion state 8192), so macOS App Naps it about 27 to 30 s after process launch. App Nap does not slow timers here (a 16 ms `setInterval` stayed at 50/s and a `MessageChannel`/`setTimeout(0)` loop kept running at tens of thousands per second throughout), but it cuts display-link callbacks to about 10 Hz. A bare `CVDisplayLink` I added inside the fixture process showed the same cut (about 61 callbacks/s to about 13/s at about 30 s) while the activity-holding run kept 61/s. WebKit paces `requestAnimationFrame` from that display link, so rAF falls from 60 to 10 to 14 Hz while `visibilityState` stays `visible`. The earlier "random 17 to 47 s" was launch-relative: with the window ordered only at launch+30 s the page was already throttled at its first sample (`preplace30`). Earlier "NSProcessInfo activity did not help" did not reproduce: held from fixture startup or at placement it held 60 Hz in every run below.

**Fix: hold `-[NSProcessInfo beginActivityWithOptions:reason:]` with `NSActivityUserInitiatedAllowingIdleSystemSleep` (0x00EFFFFF) for the life of the hosting app.** Public API, no private SPI. `NSActivityLatencyCritical` alone and `NSActivityBackground` do not help; the user-initiated bit is the one that matters. The `latencyCritical` bit used in the first try is not required. Starting the activity after the drop recovers rAF within 2 s (`late_napoff`, started at 45 s, 60 Hz from 46 s), so it is also safe to apply late. It must live in the process that owns the window (rightkit-control's embedded path can do this; an uncooperating third-party app cannot be fixed from outside).

All runs: fixture off-screen at x=3600, `_setWindowOcclusionDetectionEnabled:NO` applied, page-only measurement (single read at the end unless noted), rAF counted in the page every 2 s (1 s in the monitor).

| # | Hypothesis / change | Measured rAF (Hz over time) | Verdict |
|---|---|---|---|
| 1 | Baseline: SPI only, no polling, 120 s (x3), polled every 3 s (x1), 70 s with `sample` | 60 until 26 to 30 s, then 9 to 14 for the rest; 5 of 5 dropped | Reproduces, launch-relative |
| 2 | Process throttling / timer coalescing (16 ms interval, MessageChannel loop sampled with the rAF) | timers flat at 50/s and loop at tens of thousands/s while rAF fell to about 11 | Not process or timer throttling; display link only |
| 3 | Re-`orderFrontRegardless` at 20 s | still dropped at 27 s | No effect |
| 4 | `WKPreferences` `_setPageVisibilityBasedProcessSuppressionEnabled:NO`, `_setHiddenPageDOMTimerThrottlingEnabled:NO`, `_setDOMTimersThrottlingEnabled:NO`, `_setAppNapEnabled:NO` (all on the live preferences, accepted) | dropped at 26 s | Does not help (and none are needed) |
| 5 | Delay `place` to launch+30 s | page already at 10 to 13 Hz at first sample | Drop is launch-relative, not window-relative |
| 6 | `NSAppSleepDisabled=YES` via process-scoped `registerDefaults` (in-memory, nothing written), 120 s | 60 Hz for all 120 s, 3 runs (one with 3 s polling) | Fixes it, but undocumented key; not needed |
| 7 | `beginActivity` `UserInitiatedAllowingIdleSystemSleep` + `LatencyCritical`, 120 s | 60 Hz for all 120 s, 4 runs | Fixes it |
| 8 | `UserInitiatedAllowingIdleSystemSleep` only, 90 s | 60 Hz for 90 s | Fixes it; this is the minimal option |
| 9 | `LatencyCritical` only / `Background` only, 90 s | dropped at 26 s | Insufficient |
| 10 | Activity started late (45 s, after the drop) | 10 to 14 Hz 27 to 45 s, 60 Hz from 46 s on | Recovers |
| 11 | Window occlusion state over time (polled `occlusionState` every 3 s) | constant 8192 (not visible), no flipping | Not the cause |
| 12 | WebKit `inactiveSchedulingPolicy=.none`, process-pool or screen-nil fallbacks | `policy` was earlier shown ineffective; the `CVDisplayLink` fires at 60 Hz on the active display regardless of `screen == nil`, so screen-nil binding is not the cause | Not the cause |
| 13 | `CADisplayLink` from `-[NSWindow displayLinkWithTarget:selector:]` / `-[NSView ...]` | never fired (0 callbacks) in either state | Not usable off-screen; not required |
| 14 | RunningBoard log (`log stream` for the WebContent pid) | stream produced no events for the fixture in this session | Inconclusive; superseded by the measured result |

Not run: `LSAppNapIsDisabled` Info.plist (the fixture runs as a bare binary because `.app` copies stopped creating their window; hypothesis 6 is the same App Nap switch via defaults); `_setVisibleOverride`-style WebKit SPI and `_setThrottleStateForTesting:` (unneeded once the display link is fixed, listed as the untested items in the open defect above).

### Re-run of the spike criterion with the fix

`RK2_NAP` default is now on in `run.py` (`nap_off` command with the default options before `place`; `RK2_NAP=off` restores the old behaviour); evidence in `*-napfix` dirs, the old defective runs are kept.

| Run | Frames | Black | OCR step id correct | Counter advanced, whole run | rAF samples | First throttled | Foreign magenta |
|---|---|---|---|---|---|---|---|
| offscreen 30 fps, 60 s (`offscreen-30fps-stream-napfix`) | 1760 | 0 | 277/277 | 1085/1085 = 100.0% | 60 Hz throughout | none | 0 |
| offscreen 60 fps, 60 s (`offscreen-60fps-stream-napfix`) | 3343 | 0 | 432/432 | 2079/2083 = 99.8% | 60 Hz throughout | none | 0 |
| occluded 30 fps, 120 s (`occluded-30fps-stream-napfix`) | 3521 | 0 | 394/394 | 2165/2165 = 100.0% | 60 Hz for 120 s | none | 0 |

The "counter advances in at least 95% of intervals for 60 s" criterion **passes** for off-screen-visible at 30 and 60 fps and for occluded (120 s). Page-only 120 s holds also passed (hypotheses 6 to 8 above). The earlier 52 to 58% whole-run figure came entirely from the post-throttle segment.

### Decision table update (macOS)

| Visibility state | Verdict now |
|---|---|
| Off-screen-visible | **Supported.** Defect closed. Fixture-side requirements: constrain override (class-scoped), `_setWindowOcclusionDetectionEnabled:NO` before first order, **hold an NSProcessInfo `UserInitiatedAllowingIdleSystemSleep` activity in the hosting process** |
| Occluded (off-screen overlap) | **Supported**, R12 still 0 foreign pixels, rAF 60 Hz for 120 s |
| App-hidden, minimized, no-SPI, direct-to-file | unchanged (unsupported or unsuitable) |

### Private SPI and release risk

- `-[WKWebView _setWindowOcclusionDetectionEnabled:]` is still the only private SPI (the rAF fix adds none). It is an underscore-prefixed method on a public class: App Store review rejects non-public API use, but notarized Developer ID distribution (Sparkle/GitHub or R2) does not check for it. Risk is breakage, not rejection: it is called through `respondsToSelector:` and reported if missing (the fixture says `spi selector missing`), and the failure mode is loud (page goes hidden, rAF 0, grey frames). Pin it to a qualification test on each macOS major, gating `launch_offscreen`.
- The constrain override replaces `constrainFrameRect:toScreen:` (public, overridden by `class_replaceMethod` on the window class), which is a runtime-swizzle, not private API, but affects every window of that class in the process; keep it scoped to the Tao window class of the driven app.
- `beginActivity` is public. Holding it costs idle-system-sleep permission only (the options allow idle sleep); it does keep the app out of App Nap for as long as the driven session lasts, so release it when the session ends.
- If the SPI must go: the app-owned render-surface fallback (below) removes the occlusion SPI but not the need for an activity.

### Fallback assessment (not needed now, kept as the exit)

If WebKit ever stops honouring the occlusion override: render the page into an app-owned surface (WKWebView `takeSnapshot` is too slow for 30 to 60 fps; the realistic route is hosting the webview in a `NSView` backed by a layer rendered with `CARenderer` or a `WKWebView` inside a window forced onto a virtual display, with the helper reading an `IOSurface`). Effort estimate (agentic wall-clock, Mac only): about 1 to 2 h for a prototype with `CALayer` render-to-`IOSurface` and rAF measurement, 3 to 5 h to productize it into `launch_offscreen` with the helper reading the shared surface and parity tests (clock mapping, OCR, R12). Not recommended while the current path passes.
