import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { dictationStorageKey, type DeliveryTarget, type PillAction } from "../host.js";
import { listenPill, pillClient } from "./pillHost.js";
import { modeFor, type Mode } from "./pillBars.js";

const PROMPT_HINT_COUNT_STORAGE_ID = () => dictationStorageKey("promptHintCount");
const PROMPT_HINT_SEEN_STORAGE_ID = () => dictationStorageKey("promptHintSeen");
const PROMPT_HINT_DISABLED_STORAGE_ID = () => dictationStorageKey("promptHintDisabled");
const PROMPT_HINT_THRESHOLD = 3;
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
  intro: boolean;
  setIntro: (intro: boolean) => void;
  introFired: RefObject<boolean>;
  modeRef: RefObject<Mode>;
  hoveredRef: RefObject<boolean>;
  nativeHoveredRef: RefObject<boolean>;
  hoverExitDelayRef: RefObject<number | null>;
  clearHoverExit: () => void;
  dispatch: (cmd: PillAction) => void;
  openClipboard: () => void;
}

export interface PillLabels {
  status?: string;
  success?: string;
  error?: string;
}

export function usePillEvents(labels: PillLabels = {}): PillEvents {
  const [mode, setMode] = useState<Mode>("idle");
  const [hovered, setHovered] = useState(false);
  const [modelPreparing, setModelPreparing] = useState(false);
  const [statusLabel, setStatusLabel] = useState(labels.status ?? "");
  const [errorLabel, setErrorLabel] = useState(labels.error ?? "");
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
  recStart: RefObject<number | null>;
  levelRef: RefObject<number>;
  labels?: PillLabels;
}

function usePillModeSubscription({
  labels,
  modeRef,
  recStart,
  levelRef,
  setMode,
  setStatusLabel,
  setErrorLabel,
}: {
  labels: PillLabels;
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
          setStatusLabel(ev.label.trim() || labels.status || "");
        } else if (ev.kind === "success") {
          setStatusLabel(labels.success ?? "");
        } else if (ev.kind === "error") {
          setErrorLabel(ev.label.trim() || labels.error || "");
        }
        if (next === "recording") {
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
  }, [recStart, setErrorLabel, setMode, setStatusLabel, labels.status, labels.success, labels.error]);
}

function usePillPromptHintSubscription(): void {
  useEffect(() => {
    const logPromptHint = (message: string) => {
      void pillClient().log("prompt_hint_trigger", message).catch(() => undefined);
    };
    let latestSequence = 0;
    const unlisten = listenPill("deliveryRecorded", (event) => {
      const deliveryId = event.payload.delivery_id;
      if (!deliveryId) {
        logPromptHint("reason=missing_delivery_id");
        return;
      }
      const sequence = ++latestSequence;
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
            logPromptHint("reason=storage_unavailable action=open");
          }
          void pillClient()
            .showPromptHint()
            .then(() => {
              try {
                localStorage.setItem(PROMPT_HINT_SEEN_STORAGE_ID(), "1");
              } catch {
                // Storage is best effort after successful native presentation.
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

function usePillTerminalResetSubscription({
  mode,
  hovered,
  setMode,
}: {
  mode: Mode;
  hovered: boolean;
  setMode: (mode: Mode) => void;
}): void {
  useEffect(() => {
    if (mode !== "status" && mode !== "error") return;
    if (mode === "error" && hovered) return;
    const hold = mode === "status" ? 650 : 5000;
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
      const command = mode === "error" ? "recover_terminal_error" : "reveal_pill";
      void (mode === "error" ? pillClient().recoverTerminalError(generation) : pillClient().revealPill()).catch((error) => {
        void pillClient().log("pill_terminal_reset_error", `command=${command} error=${String(error).slice(0, 240)}`);
      });
    }, hold);
    return () => window.clearTimeout(id);
  }, [mode, hovered, setMode]);
}

function usePillIntroSubscription({
  introFired,
  setIntro,
}: {
  introFired: RefObject<boolean>;
  setIntro: (intro: boolean) => void;
}): void {
  // biome-ignore lint/correctness/useExhaustiveDependencies: introFired is a stable launch latch.
  useEffect(() => {
    const runIntro = () => {
      if (introFired.current) return;
      introFired.current = true;
      void pillClient().takeIntro().catch(() => undefined);
      void pillClient().log("pill_intro_received", "starting").catch(
        () => undefined,
      );
      setIntro(true);
    };
    const unlisten = listenPill("intro", runIntro);
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

function usePillCursorHighlightSubscription(): void {
  useEffect(() => {
    let current: Element | null = null;
    let lastX = Number.NaN;
    let lastY = Number.NaN;
    const setHot = (btn: Element | null) => {
      if (btn === current) return;
      current?.removeAttribute("data-pill-hovered");
      if (btn) btn.setAttribute("data-pill-hovered", "true");
      current = btn;
    };
    const unlisten = listenPill("cursor", (e) => {
      const [x, y] = e.payload;
      if (x === lastX && y === lastY) return;
      lastX = x;
      lastY = y;
      const el = document.elementFromPoint(x, y);
      setHot(el ? el.closest("[data-pill-action]") : null);
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
  { recStart, levelRef, labels = {} }: PillEventSources,
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

  usePillModeSubscription({ labels, modeRef, recStart, levelRef, setMode, setStatusLabel, setErrorLabel });
  usePillPromptHintSubscription();
  usePillTerminalResetSubscription({ mode, hovered, setMode });
  usePillIntroSubscription({ introFired, setIntro });
  usePillHoverSubscription({ hoveredRef, nativeHoveredRef, hoverExitDelayRef, clearHoverExit, setHovered });
  usePillCursorHighlightSubscription();
}
