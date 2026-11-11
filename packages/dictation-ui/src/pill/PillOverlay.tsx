// PillOverlay — the floating dictation pill, in the locked the host app identity.
//
// The pill's accent is BLUE — its own cool identity on the
// live voice surface; ember stays the app brand elsewhere. State reads by shape:
//   idle       — slim resting pill with a visible blue ring; hover expands it
//   recording  — orange bars + Tanker timer; the blue border stays static;
//                hover → stop/cancel/send
//   processing — small round pill, blue spinner, gently pulsing blue rim
//   status     — short text result ("Pasted", "Sent", "Saved", "Copied", ...)
//   error      — red actionable audio/device error capsule with warning icon
// status/error auto-return to idle (error pauses while hovered so it is readable).
// The mini clipboard is hosted outside this visible pill window: both macOS and
// Windows open the shared React clipboard in a separate transient popup above it.
//
// The macOS window is a non-activating NSPanel (clicks don't steal focus from the
// target app). A ResizeObserver reports the pill's content box to the Rust
// `pill_resize` command, which keeps the window hugging the content, bottom-
// anchored (so hover panels grow upward). Native pills must mirror this React
// treatment for parity. The current Win32 renderer is legacy plumbing only; do
// not copy its visual treatment.
//
// Decomposition: this file is view composition + JSX only —
//   • usePillEvents owns the mode/hover/label/intro state surface, the mirror
//     refs, and the outbound command channel;
//   • usePillEventSubscriptions (same file) owns every Rust-driven
//     subscription and its associated timers: the pill://event mode
//     transitions + recording-timer origin, the delivery-recorded prompt-hint
//     flow, the status/error auto-return (which invokes
//     `recover_terminal_error` on error expiry — logging
//     `pill_terminal_reset_error` on failure — and `reveal_pill` after
//     status), the once-per-launch pill://intro latch, the pill://hover leave
//     grace, and the pill://cursor/.jshover highlight;
//   • usePillAnimation owns the rAF bar loop + recording clock + intro bloom
//     mechanics;
//   • usePillGeometry owns the window surface, reportPillHitbox, the
//     ResizeObserver auto-size push, and the [48,140,260]ms geometry-settle
//     re-report fan-out.
// Those effect bodies remain contract-pinned — see
// scripts/test-pill-hover-contract.mjs, test-delivery-clipboard-contract.mjs,
// test-frontend-repairs.mjs, test-20260726-release-regressions.mjs,
// assert-release-contracts.mjs and src-tauri/tests/
// pill_lifecycle_contract_tests.rs before editing them. NOTE: the Rust
// contract test pins the literal `recover_terminal_error` and
// `pill_terminal_reset_error` strings to THIS file — keep them present in
// this comment if the code itself stays in usePillEvents.ts.

import { getPlatform } from "@rightkit/platform-ui";
import { Icon } from "./PillIcon.js";
import {
  BAR_BUFFER_SIZE,
  mmss,
  processingBarPixelHeight,
  processingDisplayBars,
  recordingBarPixelHeight,
  recordingDisplayBars,
} from "./pillBars.js";
import { CSS } from "./pillStyles.js";
import { usePillAnimation } from "./usePillAnimation.js";
import { usePillEvents, usePillEventSubscriptions } from "./usePillEvents.js";
import { usePillGeometry } from "./usePillGeometry.js";

// Rendered <i> bar nodes carry heights from this constant buffer — the rAF
// loop owns real values via direct DOM writes (see usePillAnimation).
const ZERO_BARS = new Array(BAR_BUFFER_SIZE).fill(0);

export default function PillOverlay() {
  const events = usePillEvents();
  const {
    mode,
    hovered,
    statusLabel,
    errorLabel,
    modelPreparing,
    intro,
    setHovered,
    modeRef,
    hoveredRef,
    nativeHoveredRef,
    clearHoverExit,
    dispatch,
    openClipboard,
  } = events;
  const { wrapRef, pillRef } = usePillGeometry({
    mode,
    hovered,
    statusLabel,
    modelPreparing,
    modeRef,
    hoveredRef,
  });
  const { recStart, levelRef, recBarsRef, procBarsRef, timerRef } = usePillAnimation(mode);
  usePillEventSubscriptions(events, { pillRef, recStart, levelRef });

  // One PERSISTENT .hr-pill element across every mode — children swap, the
  // element does not. Keeping one capsule avoids mount/unmount races between
  // mode content and preserves the measured hitbox while geometry changes.
  const cancelled = mode === "status" && statusLabel.trim().toLowerCase() === "cancelled";
  const pillClass = `hr-pill ${
    mode === "idle"
      ? `idle${modelPreparing ? " preparing" : ""}${intro ? " intro" : ""}`
      : mode === "recording"
        ? "recording"
        : mode === "starting"
          ? "processing"
          : mode === "processing"
            ? "processing"
            : mode === "status"
              ? `status${cancelled ? " cancelled" : ""}`
              : "error"
  }${hovered && mode !== "status" ? " hover" : ""}`;

  // Non-visual state perception: every mode gets a role + accessible name so
  // a screen-reader user can tell idle/listening/transcribing/error apart
  // without seeing the pill. Previously only "status"/"starting" announced
  // anything — idle, recording, and error were silent. `status` keeps its
  // existing behavior (the visible <span> text below IS its accessible
  // name/announcement, e.g. "Saved"/"Cancelled"), so it gets no aria-label
  // override here. Labels are deliberately static text, not live values (no
  // per-second elapsed time in "Listening") so the live region doesn't spam
  // an assistive technology user every tick.
  const pillRole = mode === "error" ? "alert" : "status";
  const pillAriaLabel =
    mode === "idle"
      ? modelPreparing
        ? "Preparing"
        : "Idle"
      : mode === "recording"
        ? "Listening"
        : mode === "starting"
          ? "Starting microphone"
          : mode === "processing"
            ? "Transcribing"
            : mode === "error"
              ? errorLabel
              : undefined; // "status" mode: accessible name comes from its own text content.

  return (
    // `win` only bottom-anchors the content inside the Windows overlay frame.
    // Pill colour and rim treatment are deliberately shared with macOS.
    <div className={`hr-pill-root${getPlatform() === "windows" ? " win" : ""}`}>
      <style>{CSS}</style>
      <div ref={wrapRef} className="hr-wrap">
        <div
          ref={pillRef}
          className={pillClass}
          role={pillRole}
          aria-live={pillRole === "alert" ? undefined : "polite"}
          tabIndex={mode === "idle" || mode === "recording" ? 0 : -1}
          onFocusCapture={() => {
            clearHoverExit();
            hoveredRef.current = true;
            setHovered(true);
          }}
          onBlurCapture={(event) => {
            if (event.currentTarget.contains(event.relatedTarget as Node | null)) return;
            // Keyboard focus may leave for a separate popup while native hover
            // still owns expansion. Collapsing here moves that popup mid-click.
            clearHoverExit();
            hoveredRef.current = nativeHoveredRef.current;
            setHovered(nativeHoveredRef.current);
          }}
        >
          {pillAriaLabel ? <span className="sr-only">{pillAriaLabel}</span> : null}
          {mode === "idle" && (
            <>
              <span className="hr-hover-anchor" aria-hidden="true" />
              <span className="hr-actions hr-idle-actions" aria-hidden={!hovered}>
                {modelPreparing ? (
                  <>
                    <span className="hr-spinner" aria-hidden="true" />
                    <span className="hr-prep-label">Preparing</span>
                  </>
                ) : (
                  <>
                    <button
                      type="button"
                      className="hr-btn c-ember"
                      aria-label="Open hub"
                      title="Open hub"
                      data-tooltip="Open hub"
                      onClick={() => dispatch("openHubHome")}
                    >
                      <Icon name="hub" />
                    </button>
                    <button
                      type="button"
                      className="hr-btn c-green play"
                      aria-label="Start dictation"
                      title="Start dictation"
                      data-tooltip="Start dictation"
                      onClick={() => {
                        dispatch("startDictation");
                      }}
                    >
                      <Icon name="mic" />
                    </button>
                  </>
                )}
                <button
                  type="button"
                  className="hr-btn c-white"
                  aria-label="Recent transcripts"
                  title="Recent transcripts"
                  data-tooltip="Recent transcripts"
                  onClick={() => void openClipboard()}
                >
                  <Icon name="list" />
                </button>
              </span>
            </>
          )}

          {mode === "recording" && (
            <>
              <span className="hr-hover-anchor" aria-hidden="true" />
              <button
                type="button"
                className="hr-btn c-red hr-hover-control"
                aria-label="Cancel"
                title="Cancel"
                data-tooltip="Cancel"
                aria-hidden={!hovered}
                onClick={() => dispatch("cancelDictation")}
              >
                <Icon name="cancel" />
              </button>
              <span className="hr-bars" aria-hidden="true" ref={recBarsRef}>
                {recordingDisplayBars(ZERO_BARS).map((_, index) => (
                  <i
                    // biome-ignore lint/suspicious/noArrayIndexKey: fixed-length positional slice (always RECORDING_BAR_COUNT bars); a bar's index IS its identity and the list never reorders or splices
                    key={`rec-bar-${index}`}
                    style={{ height: `${recordingBarPixelHeight(0)}px` }}
                  />
                ))}
              </span>
              <span className="timer" ref={timerRef}>{mmss(0)}</span>
              <span className="hr-actions hr-recording-actions" aria-hidden={!hovered}>
                <button
                  type="button"
                  className="hr-btn c-orange"
                  aria-label="Stop"
                  title="Stop"
                  data-tooltip="Stop"
                  onClick={() => dispatch("stopDictation")}
                >
                  <Icon name="stop" />
                </button>
                <button
                  type="button"
                  className="hr-btn c-green"
                  aria-label="Stop and send"
                  title="Stop and send"
                  data-tooltip="Stop and send"
                  onClick={() => dispatch("stopAndSend")}
                >
                  <Icon name="send" />
                </button>
              </span>
            </>
          )}

          {(mode === "starting" || mode === "processing") && (
            <span
              ref={procBarsRef}
              className="hr-wave"
              role="img"
              aria-label={mode === "starting" ? "Starting microphone" : "Processing"}
            >
              {processingDisplayBars(ZERO_BARS).map((_, index) => (
                <i
                  // biome-ignore lint/suspicious/noArrayIndexKey: fixed-length positional slice (always PROCESSING_BAR_COUNT bars); a bar's index IS its identity and the list never reorders or splices
                  key={`proc-bar-${index}`}
                  style={{ height: `${processingBarPixelHeight(0)}px` }}
                />
              ))}
            </span>
          )}

          {/* Status stays text-only; errors match macOS with a red capsule,
              warning icon, and visible actionable label. */}
          {mode === "status" && <span className="hr-status-label">{statusLabel}</span>}

          {mode === "error" && (
            <span className="hr-error-content">
              <span className="hr-ico">
                <Icon name="warning" />
              </span>
              <span className="hr-error-label">{errorLabel}</span>
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
