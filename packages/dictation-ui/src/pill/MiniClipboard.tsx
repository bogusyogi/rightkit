import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { ArrowClockwise, ArrowCounterClockwise, X } from "@phosphor-icons/react";
import { invoke } from "../lib/tauri.js";
import { pillTooltipProps } from "./pillTooltip.js";
import { hostAppName } from "../appName.js";

export interface MiniClipboardItem {
  delivery_id: string;
  transcript: string;
  raw_transcript?: string | null;
}

export type MiniClipboardMode = "single" | "recent";

const EDITOR_MIN_HEIGHT = 30;
// Wide and short, matching the native macOS card (ClipboardSurface): a long
// dictation reads across the width and stops, instead of growing into a tall
// column. 160px is about nine lines at 12.5px/1.35.
const EDITOR_MAX_HEIGHT = 160;

interface Props {
  items: MiniClipboardItem[];
  loaded?: boolean;
  mode: MiniClipboardMode;
  onModeChange?: (mode: MiniClipboardMode, items: MiniClipboardItem[]) => void;
  onClose?: () => void;
  onAfterOpenHub?: () => void;
  onEditInteraction?: () => void;
  closeAfterCopy?: boolean;
}

function withTimeout<T>(promise: Promise<T>, ms = 1500): Promise<T> {
  let timer: number | undefined;
  return Promise.race([
    promise,
    new Promise<never>((_, reject) => {
      timer = window.setTimeout(() => reject(new Error("timeout")), ms);
    }),
  ]).finally(() => {
    if (timer !== undefined) window.clearTimeout(timer);
  });
}

export const MINI_CLIPBOARD_CSS = `
/* The card's width must NEVER derive from the viewport: the window is sized to
   hug this card, so a 100vw-based max-width is a feedback loop. With a
   persistent (non-recreated) window it collapses — window 320 -> max-width 276
   -> window 298 -> ... until rows crush to one character (field bug
   2026-07-26). Fixed design widths keep the round-trip self-correcting. */
.hr-clip{
  width:320px; max-width:320px; flex:none; margin-bottom:7px; padding:6px; border-radius:18px;
  background:#000; color:#f5f2ee;
  border:1px solid rgba(51,51,51,.72);
  display:flex; flex-direction:column; gap:4px;
  /* Swift CenteredAuxiliaryEntrance: 0.965 -> 1 with PillMotion.settled. */
  animation:hrClipIn 150ms var(--hr-aux-settled-spring) both;
}
.hr-clip.single{width:fit-content;max-width:620px;flex:none;background:#000}
.hr-clip.single:not(.expanded){
  border-radius:999px; padding:4px 5px 4px 7px;
}
.hr-clip.single:not(.expanded) .hr-clip-list{width:auto}
.hr-clip.single:not(.expanded) .hr-clip-editor{align-items:center}
.hr-clip.single:not(.expanded) .hr-clip-field{
  height:30px; border-radius:999px; padding:6px 12px;
}
.hr-clip.single.expanded{border-radius:18px}
.hr-clip.single.expanded .hr-clip-editor{flex-direction:column;align-items:stretch}
.hr-clip.single.expanded .hr-clip-tools{align-self:flex-end;padding:0 2px 1px}
@keyframes hrClipIn{from{opacity:0;transform:scale(.965)}to{opacity:1;transform:scale(1)}}
.hr-clip-list{width:100%;display:flex;flex-direction:column;gap:4px;margin:0;padding:0;list-style:none}
.hr-clip-row{
  width:100%;min-height:34px;padding:0 10px;border:0;border-radius:12px;
  display:flex;align-items:center;gap:10px;background:#171717;color:#f5f2ee;
  text-align:left;cursor:pointer;
  transition:background-color 100ms ease-out,transform 100ms ease-out;
}
.hr-clip-row-text{
  flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:normal;
  display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;
  font-size:12px;line-height:1.3;font-weight:620;color:#f5f2ee;
}
.hr-clip-row-action{
  flex:none;opacity:0;transform:translateX(2px);color:rgba(148,190,255,.96);
  font:740 10.5px/1 'Hanken Grotesk',system-ui,sans-serif;
  transition:opacity 120ms ease,transform 120ms ease;
}
.hr-clip-row:hover,.hr-clip-row:focus-visible{background:rgba(255,255,255,.14)}
.hr-clip-row:hover .hr-clip-row-action,.hr-clip-row:focus-visible .hr-clip-row-action,.hr-clip-row.copied .hr-clip-row-action{opacity:1;transform:none}
.hr-clip-row:active{transform:scale(.99)}
.hr-clip-row:focus-visible{outline:2px solid rgba(148,190,255,.46);outline-offset:-2px}
.hr-clip-empty{min-width:120px;padding:10px 8px;text-align:center;font-size:12px;color:var(--muted);list-style:none}
.hr-clip-editor{width:auto;max-width:100%;display:flex;align-items:center;gap:4px;list-style:none}
.hr-clip-field{
  flex:none; min-width:0; max-width:440px; height:30px; box-sizing:border-box; resize:none; overflow:hidden;
  border:1px solid transparent; border-radius:12px; background:#171717; color:#f5f2ee;
  font:650 12.5px/1.35 'Hanken Grotesk',system-ui,sans-serif; padding:7px 10px;
  outline:none; user-select:text; -webkit-user-select:text;
  transition:background-color 100ms ease-out, border-color 100ms ease-out;
}
.hr-clip-field.expanded{overflow-y:auto;overflow-x:hidden}
.hr-clip-field:focus{background:rgba(255,255,255,.085);border-color:rgba(90,160,255,.38);caret-color:rgba(247,244,239,.94)}
.hr-clip-tools,.hr-clip-footer{display:flex;align-items:center;gap:2px;flex:none}
.hr-clip-footer{justify-content:flex-end;padding-top:2px}
.hr-clip-icon,.hr-clip-link{
  position:relative; border:0; cursor:pointer; background:transparent; color:var(--muted);
  transition:background-color 100ms ease-out,color 100ms ease-out,transform 100ms ease-out;
}
.hr-clip-icon{width:26px;height:26px;padding:0;border-radius:50%;display:inline-grid;place-items:center}
.hr-clip-icon svg{width:15px;height:15px}
.hr-clip-icon.ai-toggle{color:rgba(148,190,255,.96)}
.hr-clip-icon.close{color:rgba(229,103,91,.88)}
.hr-clip-icon:hover,.hr-clip-link:hover{background:rgba(255,255,255,.14);color:#f5f2ee}
.hr-clip-icon:disabled{cursor:not-allowed;opacity:.42;transform:none}
.hr-clip-icon:disabled:hover{background:transparent;color:var(--muted)}
.hr-clip-icon[aria-disabled="true"]{cursor:not-allowed;opacity:.42;transform:none}
.hr-clip-icon[aria-disabled="true"]:hover{background:transparent;color:var(--muted)}
.hr-clip-icon.close:hover{background:rgba(229,103,91,.13);color:#ef7a70}
.hr-clip-icon:active,.hr-clip-link:active{transform:scale(.97)}
.hr-clip-icon:focus-visible,.hr-clip-link:focus-visible{outline:2px solid rgba(148,190,255,.46);outline-offset:-2px}
.hr-clip-link{height:24px;padding:0 8px;border-radius:999px;font:740 11px/1 'Hanken Grotesk',system-ui,sans-serif}
.hr-clip-link.copy-action{min-width:44px;color:rgba(148,190,255,.96)}
@media(prefers-reduced-motion:reduce){.hr-clip{animation:none}.hr-clip-field,.hr-clip-row,.hr-clip-row-action,.hr-clip-icon,.hr-clip-link{transition:none}}
`;

async function openHub(tab: "home" | "history") {
  await invoke(tab === "history" ? "open_hub_history" : "open_hub_home").catch(() => undefined);
}

// Memoized per-row component for the "recent" list (up to 5 items) so a
// copiedId tick on ONE row doesn't re-render the others.
const ClipRow = memo(function ClipRow({
  item,
  isCopied,
  onCopy,
}: {
  item: MiniClipboardItem;
  isCopied: boolean;
  onCopy: (item: MiniClipboardItem) => Promise<void>;
}) {
  return (
    <li>
      <button
        className={isCopied ? "hr-clip-row copied" : "hr-clip-row"}
        type="button"
        aria-label={isCopied ? "Copied" : "Copy transcript"}
        {...pillTooltipProps("pill_clipboard", isCopied ? "Copied" : "Click to copy")}
        onClick={() => void onCopy(item)}
      >
        <span className="hr-clip-row-text">{item.transcript}</span>
        <span className="hr-clip-row-action">{isCopied ? "Copied" : "Copy"}</span>
      </button>
    </li>
  );
});

export default function MiniClipboard({
  items,
  loaded = true,
  mode,
  onModeChange,
  onClose,
  onAfterOpenHub,
  onEditInteraction,
  closeAfterCopy = false,
}: Props) {
  const [copiedId, setCopiedId] = useState("");
  // The current (single) row is editable so a misheard word can be fixed before it
  // lands in the clipboard. `null` = untouched → use the item's own transcript.
  const current = mode === "single" ? items[0] : undefined;
  // Draft is tagged with the item id it belongs to, so it auto-resets when the
  // current transcript changes - no effect, no stale sync.
  const [draft, setDraft] = useState<{ id: string; text: string; dirty: boolean } | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [editorHeight, setEditorHeight] = useState(EDITOR_MIN_HEIGHT);
  const fieldRef = useRef<HTMLTextAreaElement | null>(null);
  // Copied-flash / close-after-copy timers: tracked so an unmount (pill
  // dismiss) clears them instead of writing state on a dead component.
  const pendingTimers = useRef<Set<number>>(new Set());
  const schedule = useCallback((fn: () => void, ms: number) => {
    const id = window.setTimeout(() => {
      pendingTimers.current.delete(id);
      fn();
    }, ms);
    pendingTimers.current.add(id);
  }, []);
  useEffect(
    () => () => {
      for (const id of pendingTimers.current) window.clearTimeout(id);
      pendingTimers.current.clear();
    },
    [],
  );
  const editValue =
    current && draft && draft.id === current.delivery_id ? draft.text : (current?.transcript ?? "");
  const rawTranscript =
    current?.raw_transcript && current.raw_transcript.trim() !== current.transcript.trim()
      ? current.raw_transcript
      : "";
  const polishedTranscript = current?.transcript ?? "";
  const showingRaw =
    Boolean(rawTranscript) && editValue.trim() === rawTranscript.trim() && draft?.dirty !== true;
  const fieldWidth = Math.min(440, Math.max(150, Math.ceil(editValue.length * 6.4 + 18)));
  // Keep the single transcript compact by default. Expand only for genuinely
  // multi-line/long text or when the user edits into multiple lines.
  useEffect(() => {
    if (mode !== "single") return;
    setExpanded(editValue.length > 92 || editValue.includes("\n"));
  }, [mode, editValue]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: content and width changes must retrigger DOM measurement.
  useLayoutEffect(() => {
    const field = fieldRef.current;
    if (!field) return;
    if (!expanded) {
      setEditorHeight(EDITOR_MIN_HEIGHT);
      return;
    }
    field.style.height = `${EDITOR_MIN_HEIGHT}px`;
    const nextHeight = Math.min(EDITOR_MAX_HEIGHT, Math.max(EDITOR_MIN_HEIGHT, field.scrollHeight));
    field.style.height = `${nextHeight}px`;
    setEditorHeight(nextHeight);
    // Collapsing to EDITOR_MIN_HEIGHT to measure leaves the textarea scrolled
    // wherever it was clamped, so a long dictation reopened mid-text: cut off at
    // the top as well as the cap. A capped clip must start at its first word.
    if (document.activeElement !== field) field.scrollTop = 0;
  }, [expanded, editValue, fieldWidth]);

  const finishCopy = useCallback(
    (id: string) => {
      setCopiedId(id);
      if (closeAfterCopy) {
        schedule(() => onClose?.(), 260);
      } else {
        schedule(() => {
          setCopiedId((c) => (c === id ? "" : c));
        }, 1200);
      }
    },
    [closeAfterCopy, onClose, schedule],
  );

  async function copyEdited() {
    if (!current) return;
    // Read the live textarea value, not the controlled state — a webview reload /
    // item refetch can reset the React draft, which would copy the stale original.
    const value = fieldRef.current?.value ?? editValue;
    try {
      await withTimeout(invoke("copy_text_to_clipboard", { text: value }));
      // Persist only user edits. AI undo/redo is a local view toggle so redo is
      // not lost by overwriting the persisted polished transcript.
      if (draft?.dirty && value !== current.transcript) {
        onModeChange?.(
          mode,
          items.map((item) =>
            item.delivery_id === current.delivery_id ? { ...item, transcript: value } : item,
          ),
        );
        setDraft({ id: current.delivery_id, text: value, dirty: true });
        const update = withTimeout(
          invoke("update_history_item", {
            deliveryId: current.delivery_id,
            transcript: value,
          }),
          600,
        ).catch(() => undefined);
        if (current.delivery_id === "current") {
          await update;
        } else {
          void update;
        }
      }
      finishCopy(current.delivery_id);
    } catch {
      setCopiedId("");
    }
  }

  const copy = useCallback(
    async (item: MiniClipboardItem) => {
      try {
        await withTimeout(invoke("copy_text_to_clipboard", { text: item.transcript }));
        finishCopy(item.delivery_id);
      } catch {
        setCopiedId("");
      }
    },
    [finishCopy],
  );

  function toggleAiPolish() {
    if (!current || !rawTranscript) return;
    const next = showingRaw ? polishedTranscript : rawTranscript;
    setDraft({ id: current.delivery_id, text: next, dirty: false });
  }

  return (
    <section
      className={`hr-clip ${mode}${mode === "single" && expanded ? " expanded" : ""}`}
      aria-label="Recent transcripts"
    >
      <ul className="hr-clip-list">
        {!loaded ? (
          <li className="hr-clip-empty">Loading…</li>
        ) : items.length === 0 ? (
          <li className="hr-clip-empty">Nothing yet</li>
        ) : current ? (
          <li className="hr-clip-editor">
            <textarea
              ref={fieldRef}
              className={`hr-clip-field${expanded ? " expanded" : ""}`}
              value={editValue}
              onChange={(e) => {
                setDraft({ id: current.delivery_id, text: e.target.value, dirty: true });
                if (e.target.value.length > 92 || e.target.value.includes("\n")) setExpanded(true);
              }}
              aria-label="Edit current transcript"
              onPointerDown={onEditInteraction}
              onFocus={() => {
                onEditInteraction?.();
              }}
              title="Edit current transcript"
              rows={1}
              wrap={expanded ? "soft" : "off"}
              style={{ width: fieldWidth, height: editorHeight }}
            />
            <div className="hr-clip-tools">
              <button
                className="hr-clip-icon ai-toggle"
                type="button"
                aria-disabled={!rawTranscript}
                aria-label={showingRaw ? "Redo AI polish" : "Undo AI polish"}
                {...pillTooltipProps(
                  "pill_clipboard",
                  rawTranscript
                    ? showingRaw
                      ? "Redo AI polish"
                      : "Undo AI polish"
                    : "No AI polish to undo",
                )}
                onMouseDown={(event) => event.preventDefault()}
                onClick={toggleAiPolish}
              >
                {showingRaw ? (
                  <ArrowClockwise weight="bold" aria-hidden="true" />
                ) : (
                  <ArrowCounterClockwise weight="bold" aria-hidden="true" />
                )}
              </button>
              <button
                className="hr-clip-link copy-action"
                type="button"
                aria-label={copiedId === current.delivery_id ? "Copied" : "Copy transcript"}
                {...pillTooltipProps(
                  "pill_clipboard",
                  copiedId === current.delivery_id ? "Copied" : "Click to copy",
                )}
                onClick={() => void copyEdited()}
              >
                {copiedId === current.delivery_id ? "Copied" : "Copy"}
              </button>
              {onClose && (
                <button
                  className="hr-clip-icon close"
                  type="button"
                  aria-label="Close"
                  {...pillTooltipProps("pill_clipboard", "Close")}
                  onClick={(event) => {
                    event.preventDefault();
                    event.stopPropagation();
                    onClose();
                  }}
                >
                  <X weight="bold" aria-hidden="true" />
                </button>
              )}
            </div>
          </li>
        ) : (
          items.map((item) => (
            <ClipRow
              key={item.delivery_id}
              item={item}
              isCopied={copiedId === item.delivery_id}
              onCopy={copy}
            />
          ))
        )}
      </ul>
      {loaded && mode !== "single" && (
        <div className="hr-clip-footer">
          {items.length > 0 && (
            <button
              className="hr-clip-link"
              type="button"
              {...pillTooltipProps("pill_clipboard", "View more transcripts")}
              onClick={() => void openHub("history").finally(() => onAfterOpenHub?.())}
            >
              View more
            </button>
          )}
          <button
            className="hr-clip-link"
            type="button"
            {...pillTooltipProps("pill_clipboard", `Open ${hostAppName()} hub`)}
            onClick={() => {
              void openHub("home").finally(() => onAfterOpenHub?.());
            }}
          >
            Open hub
          </button>
          {onClose && (
            <button
              className="hr-clip-icon close"
              type="button"
              aria-label="Close"
              {...pillTooltipProps("pill_clipboard", "Close")}
              onClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
                onClose();
              }}
            >
              <X weight="bold" aria-hidden="true" />
            </button>
          )}
        </div>
      )}
    </section>
  );
}
