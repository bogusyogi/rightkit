// usePillEvents — the pill's mode/event state surface, its outbound command
// channel, and its Rust-driven event subscriptions, extracted from
// PillOverlay.
//
// usePillEvents owns the reactive state the pill's contract-pinned listeners
// mutate (mode/hovered/labels/intro + their mirror refs), the hover-exit
// delay timer handle, the model-preparation listener, and
// dispatch()/openClipboard() (every pill button routes commands through the
// pill_action_click log pair).
//
// usePillEventSubscriptions owns the subscriptions and timers moved here from
// PillOverlay.tsx: the pill://event mode transitions + recording-timer
// origin, the delivery-recorded prompt-hint flow, the status/error
// auto-return, the once-per-launch pill://intro latch, the pill://hover leave
// grace, and the pill://cursor/.jshover button highlight. It is a separate
// hook because it binds surfaces owned by the sibling hooks — usePillGeometry's
// pillRef (the intro bloom's target) and usePillAnimation's recStart/levelRef
// (the recording clock) — which only exist once those hooks have run, so
// PillOverlay composes it last.
//
// The moved bodies stay contract-pinned verbatim (scripts/
// test-pill-hover-contract.mjs, test-delivery-clipboard-contract.mjs,
// test-frontend-repairs.mjs, assert-release-contracts.mjs,
// src-tauri/tests/pill_lifecycle_contract_tests.rs).

import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { dictationStorageKey, type DeliveryTarget, type PillAction, type PillHistoryItem } from "../host.js";
import { listenPill, pillClient } from "./pillHost.js";
import { modeFor, type Mode, type PillEvent } from "./pillBars.js";
import { animatePillIntroBloom, playPillIntroSwell } from "./usePillAnimation.js";

// Browser-storage keys come from the host (`storageKeys`), so a host keeps its stored latches.
const PROMPT_HINT_COUNT_STORAGE_ID = () => dictationStorageKey("promptHintCount");
const PROMPT_HINT_SEEN_STORAGE_ID = () => dictationStorageKey("promptHintSeen");
const PROMPT_HINT_DISABLED_STORAGE_ID = () => dictationStorageKey("promptHintDisabled");
const PROMPT_HINT_THRESHOLD = 3;
// Keep the Swift controller's 100 ms leave grace. The native frame then returns
// directly to its compact geometry; scaling the hover shell into its centre
// created an obvious fold on Windows.
const HOVER_EXIT_DELAY_MS = 100;


export function isLikelyAiTarget(item: { target?: DeliveryTarget | null } | undefined): boolean {
  const process = item?.target?.process_name?.toLowerCase() ?? "";
  const title = item?.target?.window_title?.toLowerCase() ?? "";
  const haystack = `${process} ${title}`;
  if (!haystack.trim()) return false;
  if (
    /(codex|claude|chatgpt|openai|grok|gemini|perplexity|cursor|windsurf|copilot|cline|continue)/i.test(
      haystack,
    )
  ) {
    return true;
  }
  if (
    /(code|code-insiders|windowsterminal|wt|cmd|powershell|pwsh|conhost|wezterm|alacritty)/i.test(
      process,
    )
  ) {
    return /(codex|claude|claude code|chatgpt|openai|grok|gemini|ai|llm)/i.test(title);
  }
  if (/(chrome|msedge|firefox|brave|arc|opera)/i.test(process)) {
    return /(chatgpt|claude|grok|gemini|perplexity|openai|poe)/i.test(title);
  }
  return false;
}

export interface PillEvents {
  mode: Mode;
  setMode: (mode: Mode) => void;
  hovered: boolean;
  setHovered: (hovered: boolean) => void;
  modelPreparing: boolean;
  statusLabel: string;
  setStatusLabel: (label: string) => void;
  errorLabel: string;
  setErrorLabel: (label: string) => void;
  // Once-per-launch intro bloom flag, raised by the pill://intro latch in
  // usePillEventSubscriptions and rendered by the view's pillClass.
  intro: boolean;
  setIntro: (intro: boolean) => void;
  introFired: RefObject<boolean>;
  // Ref mirrors read/written synchronously inside the pinned listeners and by
  // usePillGeometry's geometry reporting (modeRef feeds the
  // pill_frontend_resize log; hoveredRef selects the compact-vs-expanded
  // hitbox surface).
  modeRef: RefObject<Mode>;
  hoveredRef: RefObject<boolean>;
  nativeHoveredRef: RefObject<boolean>;
  hoverExitDelayRef: RefObject<number | null>;
  clearHoverExit: () => void;
  dispatch: (cmd: PillAction) => void;
  openClipboard: () => void;
}

export function usePillEvents(): PillEvents {
  const [mode, setMode] = useState<Mode>("idle");
  const [hovered, setHovered] = useState(false);
  const [modelPreparing, setModelPreparing] = useState(false);
  const [statusLabel, setStatusLabel] = useState("Saved");
  // The backend's `{ kind: "error"; label }` event carries the actual failure
  // reason (mic permission denied, no recent delivery, etc.) but nothing ever
  // read `ev.label` — the pill rendered only a bare red glyph with no text and
  // no accessible name, so both sighted and screen-reader users got zero
  // information about what went wrong. Captured here and surfaced as the
  // error mode's aria-label (see role="alert" on the pill element).
  const [errorLabel, setErrorLabel] = useState("Error");
  const [intro, setIntro] = useState(false);
  const introFired = useRef(false);
  const modeRef = useRef<Mode>("idle");
  const hoveredRef = useRef(false);
  const nativeHoveredRef = useRef(false);
  const hoverExitDelayRef = useRef<number | null>(null);

  const clearHoverExit = useCallback(() => {
    if (hoverExitDelayRef.current !== null) {
      window.clearTimeout(hoverExitDelayRef.current);
      hoverExitDelayRef.current = null;
    }
  }, []);

  useEffect(() => {
    modeRef.current = mode;
  }, [mode]);

  useEffect(() => {
    const unlisten = listenPill("modelPhase", (e) => {
      setModelPreparing(e.payload.phase !== "installed");
    });
    return () => {
      void unlisten.then((fn) => fn()).catch(() => undefined);
    };
  }, []);

  const dispatch = useCallback((cmd: PillAction) => {
    const log = (message: string) =>
      pillClient().log("pill_action_click", message).catch(() => undefined);
    void log(`action=${cmd} phase=clicked`);
    void pillClient()
      .action(cmd)
      .then(() => log(`action=${cmd} phase=result outcome=ok`))
      .catch((error) =>
        log(`action=${cmd} phase=result outcome=error error=${String(error).slice(0, 240)}`),
      );
  }, []);

  const openClipboard = useCallback(() => {
    dispatch("openClipboard");
  }, [dispatch]);

  return {
    mode,
    setMode,
    hovered,
    setHovered,
    modelPreparing,
    statusLabel,
    setStatusLabel,
    errorLabel,
    setErrorLabel,
    intro,
    setIntro,
    introFired,
    modeRef,
    hoveredRef,
    nativeHoveredRef,
    hoverExitDelayRef,
    clearHoverExit,
    dispatch,
    openClipboard,
  };
}

export interface PillEventSources {
  // The .hr-pill capsule measured by usePillGeometry — the intro bloom writes
  // its animation styles straight onto it.
  pillRef: RefObject<HTMLDivElement | null>;
  // usePillAnimation's recording-clock origin + live mic-level sinks: the
  // pinned pill://event listener owns the writes, its rAF/timer loops own the
  // reads.
  recStart: RefObject<number | null>;
  levelRef: RefObject<number>;
}

// The pill://event mode-transition listener + recording-timer origin. Split
// out of usePillEventSubscriptions so each pinned channel is independently
// named; behaviour, dependency array, and registration order are unchanged.
function usePillModeSubscription({
  modeRef,
  recStart,
  levelRef,
  setMode,
  setStatusLabel,
  setErrorLabel,
}: {
  modeRef: RefObject<Mode>;
  recStart: RefObject<number | null>;
  levelRef: RefObject<number>;
  setMode: (mode: Mode) => void;
  setStatusLabel: (label: string) => void;
  setErrorLabel: (label: string) => void;
}): void {
  // biome-ignore lint/correctness/useExhaustiveDependencies: levelRef/modeRef are stable refs mutated inside the listener.
  useEffect(() => {
    const unlisten = listenPill("event", (e) => {
      const ev = e.payload;
      if (ev.kind === "voice") {
        // `starting` remains a backwards-compatible event kind for older app
        // shells. Current shells enter Recording immediately on accepted Start;
        // the first audio level only animates the meter.
        if (modeRef.current === "starting") {
          modeRef.current = "recording";
          setMode("recording");
          recStart.current = Date.now();
        }
        levelRef.current = Math.max(0, Math.min(1, ev.level));
        return;
      }
      const next = modeFor(ev);
      if (next) {
        if (ev.kind === "status") {
          setStatusLabel(ev.label.trim() || "Saved");
        } else if (ev.kind === "success") {
          setStatusLabel("Done");
        } else if (ev.kind === "error") {
          setErrorLabel(ev.label.trim() || "Error");
        }
        if (next === "recording") {
          // Start acknowledgement, authoritative runtime state, and the first
          // audio callback can all report Recording. Preserve the first origin
          // so duplicate acknowledgements never restart the visible timer.
          if (modeRef.current !== "recording" || recStart.current === null) {
            recStart.current = Date.now();
          }
        } else {
          recStart.current = null;
        }
        modeRef.current = next;
        setMode(next);
        if (next !== "recording") {
          levelRef.current = 0;
        }
      }
    });
    return () => {
      void unlisten.then((fn) => fn()).catch(() => undefined);
    };
  }, [recStart, setErrorLabel, setMode, setStatusLabel]);
}

// The delivery-recorded prompt-hint flow: decides whether the newest delivery
// warrants offering the "open in AI chat" hint, gated on the delivery's
// target, the once-per-launch/disabled localStorage latches, and the
// recordings-before-hint threshold.
function usePillPromptHintSubscription(): void {
  useEffect(() => {
    const logPromptHint = (message: string) => {
      void pillClient().log("prompt_hint_trigger", message).catch(() => undefined);
    };
    // Only the newest delivery may decide the hint: history replies can
    // resolve out of order, and a stale one must not judge a newer target.
    let latestSequence = 0;
    const unlisten = listenPill("deliveryRecorded", (event) => {
      const deliveryId = event.payload.delivery_id;
      if (!deliveryId) {
        logPromptHint("reason=missing_delivery_id");
        return;
      }
      const sequence = ++latestSequence;
      // Prefer target metadata carried on the event; fall back to the history
      // scan only for events that predate it.
      const resolveTarget: Promise<{ target?: DeliveryTarget | null } | undefined> = event.payload
        .target
        ? Promise.resolve({ target: event.payload.target })
        : pillClient().recentHistory().then((history) =>
            history.find((row) => row.delivery_id === deliveryId),
          );
      void resolveTarget
        .then((item) => {
          if (sequence !== latestSequence) {
            logPromptHint("reason=superseded");
            return;
          }
          if (!item) {
            logPromptHint("reason=delivery_not_in_history");
            return;
          }
          if (!isLikelyAiTarget(item)) {
            logPromptHint("reason=not_ai_target");
            return;
          }
          try {
            if (localStorage.getItem(PROMPT_HINT_DISABLED_STORAGE_ID()) === "1") {
              logPromptHint("reason=disabled");
              return;
            }
            if (localStorage.getItem(PROMPT_HINT_SEEN_STORAGE_ID()) === "1") {
              logPromptHint("reason=already_shown");
              return;
            }
            const next = Number(localStorage.getItem(PROMPT_HINT_COUNT_STORAGE_ID()) ?? "0") + 1;
            localStorage.setItem(PROMPT_HINT_COUNT_STORAGE_ID(), String(next));
            if (next < PROMPT_HINT_THRESHOLD) {
              logPromptHint(`reason=below_threshold count=${next}`);
              return;
            }
          } catch {
            // Storage denial should not suppress the hint forever.
            logPromptHint("reason=storage_unavailable action=open");
          }
          void pillClient()
            .showPromptHint()
            .then(() => {
              try {
                localStorage.setItem(PROMPT_HINT_SEEN_STORAGE_ID(), "1");
              } catch {
                // The popup opened; failure to persist only means it may be offered again.
              }
              logPromptHint("outcome=opened");
            })
            .catch((error) => {
              logPromptHint(`outcome=error detail=${String(error)}`);
            });
        })
        .catch((error) => {
          logPromptHint(`reason=history_error detail=${String(error)}`);
        });
    });
    return () => {
      void unlisten.then((fn) => fn()).catch(() => undefined);
    };
  }, []);
}

// The status/error auto-return-to-idle timer, plus the backend terminal-reset
// command it drives (recover_terminal_error / reveal_pill).
function usePillTerminalResetSubscription({
  mode,
  hovered,
  setMode,
}: {
  mode: Mode;
  hovered: boolean;
  setMode: (mode: Mode) => void;
}): void {
  // Status/error are momentary confirmations and return to idle on their own
  // (status is non-interactive so hover never holds it open; error pauses
  // while hovered so it stays readable).
  //
  // Processing is deliberately NOT included here: a blind timer reset could
  // drop the pill back to idle and re-expose Start while the backend is still
  // transcribing, losing an in-flight transcript. Processing ends ONLY on a
  // backend-driven `pill://event` transition (idle/status/success/error, see
  // modeFor above) — the backend's own state machine (AppState → PillEvent,
  // src-tauri/src/dictation_lifecycle.rs `app_state_to_pill_event`) is
  // authoritative for when transcription/pasting is actually done. A stuck
  // backend is covered server-side by the consumer-loop watchdog in
  // src-tauri/src/runtime_lock.rs (`TRANSCRIBE_STUCK_AFTER`), which
  // force-recovers to Armed and emits the terminal PillEvent itself.
  useEffect(() => {
    if (mode !== "status" && mode !== "error") return;
    if (mode === "error" && hovered) return;
    const hold = mode === "status" ? 650 : 5000;
    // Bind the error expiry to THIS error's generation so an expiry that fires
    // after a newer dictation/error began is a backend no-op.
    let generation: number | null = null;
    if (mode === "error") {
      void pillClient()
        .terminalErrorGeneration()
        .then((value) => {
          generation = value ?? null;
        })
        .catch(() => undefined);
    }
    const id = window.setTimeout(() => {
      setMode("idle");
      // Re-apply the visibility gate as the pill collapses back to rest. In
      // "only while recording" this hides the window (the resting pill must not
      // linger after a dictation); in "always" it just keeps the resting pill.
      // Without this the window stays shown after every cycle on macOS, which has
      // no native terminal-hide — the status/error visual returns to idle but
      // the window never hides. Error uses a strict terminal-only reset: an
      // expiry from an older Error can never cancel a newer Recording.
      const command = mode === "error" ? "recover_terminal_error" : "reveal_pill";
      void (mode === "error" ? pillClient().recoverTerminalError(generation) : pillClient().revealPill()).catch((error) => {
        void pillClient().log("pill_terminal_reset_error", `command=${command} error=${String(error).slice(0, 240)}`);
      });
    }, hold);
    return () => window.clearTimeout(id);
  }, [mode, hovered, setMode]);
}

// The once-per-launch pill://intro latch: fires the ember bloom + swell on
// backend reveal, pulling any pending latch on mount so a reveal that beat
// this listener's registration is not lost.
function usePillIntroSubscription({
  pillRef,
  introFired,
  setIntro,
}: {
  pillRef: RefObject<HTMLDivElement | null>;
  introFired: RefObject<boolean>;
  setIntro: (intro: boolean) => void;
}): void {
  // Every launch: one ember bloom inside the pill + a soft blip, so the user
  // sees and hears it came alive. Fires on REVEAL (pill://intro from Rust), not
  // on the pill's hidden mount — else the sound plays + glow animates while the
  // window is still hidden behind the splash. Once per launch.
  // biome-ignore lint/correctness/useExhaustiveDependencies: pillRef is a stable ref read inside the intro listener.
  useEffect(() => {
    // `pill://intro` is fire-and-forget with no replay, so a reveal that beats
    // this listener's registration drops the bloom entirely. Rust also
    // latches the reveal, so pull it here on mount: whichever lands first
    // wins and `introFired` keeps it to exactly one bloom per launch.
    const runIntro = () => {
      if (introFired.current) return;
      introFired.current = true;
      // A delivered event no longer needs replay. Clear the latch here so a
      // later WebView recreation cannot replay this launch over a new splash.
      // The mount-pull path already swapped it false; a second swap is harmless.
      void pillClient().takeIntro().catch(() => undefined);
      void pillClient().log("pill_intro_received", "starting").catch(
        () => undefined,
      );
      setIntro(true);
      // The bloom + swell mechanics live in usePillAnimation — rAF + inline
      // style because CSS keyframes no-op under WebView2 reduce-motion.
      animatePillIntroBloom(pillRef, setIntro);
      playPillIntroSwell();
    };
    const unlisten = listenPill("intro", runIntro);
    // Mount-time pull: covers the reveal that already happened before this
    // listener existed. No-op when the latch is clear.
    void pillClient()
      .takeIntro()
      .then((pending) => {
        if (pending) runIntro();
      })
      .catch(() => undefined);
    return () => {
      void unlisten.then((fn) => fn()).catch(() => undefined);
    };
  }, []);
}

// The pill://hover leave-grace listener: hoveredRef is the synchronous source
// of truth, setHovered(true) is immediate on enter, and setHovered(false) is
// delayed by HOVER_EXIT_DELAY_MS on leave to preserve Swift's leave grace.
function usePillHoverSubscription({
  hoveredRef,
  nativeHoveredRef,
  hoverExitDelayRef,
  clearHoverExit,
  setHovered,
}: {
  hoveredRef: RefObject<boolean>;
  nativeHoveredRef: RefObject<boolean>;
  hoverExitDelayRef: RefObject<number | null>;
  clearHoverExit: () => void;
  setHovered: (hovered: boolean) => void;
}): void {
  // Hover is driven by the Rust cursor poll (pill://hover), NOT DOM mouseenter/
  // leave — those only fire on a key window, so they break when hovering from
  // another app or after focus moves away (the "stuck expanded" bug).
  // biome-ignore lint/correctness/useExhaustiveDependencies: hoveredRef/hoverExitDelayRef are stable refs used inside the hover listener.
  useEffect(() => {
    const unlisten = listenPill("hover", (e) => {
      const next = Boolean(e.payload);
      nativeHoveredRef.current = next;
      hoveredRef.current = next;
      clearHoverExit();
      if (next) {
        setHovered(true);
        return;
      }
      // Preserve the expanded presentation for Swift's 100 ms leave grace,
      // then restore the compact native geometry without an inward scale fold.
      hoverExitDelayRef.current = window.setTimeout(() => {
        hoverExitDelayRef.current = null;
        setHovered(false);
      }, HOVER_EXIT_DELAY_MS);
    });
    return () => {
      clearHoverExit();
      void unlisten.then((fn) => fn()).catch(() => undefined);
    };
  }, [clearHoverExit, setHovered]);
}

// The pill://cursor per-button highlight: resolves the button under the
// Rust-fed pointer position and toggles the `.jshover` class the CSS mirrors,
// clearing it on hover-collapse.
function usePillCursorHighlightSubscription(): void {
  // Per-button highlight: CSS `:hover` and DOM mousemove do NOT fire on this
  // overlay until it becomes key (a click), because macOS doesn't deliver
  // mouse-move to a non-key window's WebView — and we deliberately never take
  // focus. So the Rust cursor poll FEEDS us the pointer position (`pill://cursor`,
  // client px) while hovered; we resolve the button under it with elementFromPoint
  // and toggle the `.jshover` class the CSS mirrors. This is Electron's
  // `setIgnoreMouseEvents(forward:true)` done by hand (wry has no equivalent).
  useEffect(() => {
    let current: Element | null = null;
    // Rust polls the cursor and can re-send the same client (x, y) on ticks
    // where the mouse hasn't actually moved; skip the elementFromPoint
    // hit-test in that case instead of re-resolving an identical point.
    let lastX = Number.NaN;
    let lastY = Number.NaN;
    const setHot = (btn: Element | null) => {
      if (btn === current) return;
      current?.classList.remove("jshover");
      if (btn) btn.classList.add("jshover");
      current = btn;
    };
    const unlisten = listenPill("cursor", (e) => {
      const [x, y] = e.payload;
      if (x === lastX && y === lastY) return;
      lastX = x;
      lastY = y;
      const el = document.elementFromPoint(x, y);
      setHot(el ? el.closest(".hr-btn") : null);
    });
    const unHover = listenPill("hover", (e) => {
      if (!e.payload) {
        lastX = Number.NaN;
        lastY = Number.NaN;
        setHot(null); // collapsed → clear any stuck highlight
      }
    });
    return () => {
      void unlisten.then((fn) => fn()).catch(() => undefined);
      void unHover.then((fn) => fn()).catch(() => undefined);
    };
  }, []);
}

export function usePillEventSubscriptions(
  events: PillEvents,
  { pillRef, recStart, levelRef }: PillEventSources,
): void {
  const {
    mode,
    hovered,
    setMode,
    setHovered,
    setStatusLabel,
    setErrorLabel,
    setIntro,
    introFired,
    modeRef,
    hoveredRef,
    nativeHoveredRef,
    hoverExitDelayRef,
    clearHoverExit,
  } = events;

  usePillModeSubscription({ modeRef, recStart, levelRef, setMode, setStatusLabel, setErrorLabel });
  usePillPromptHintSubscription();
  usePillTerminalResetSubscription({ mode, hovered, setMode });
  usePillIntroSubscription({ pillRef, introFired, setIntro });
  usePillHoverSubscription({ hoveredRef, nativeHoveredRef, hoverExitDelayRef, clearHoverExit, setHovered });
  usePillCursorHighlightSubscription();
}
