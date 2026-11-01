import { Button, IconButton, Checkbox } from "@rightkit/react-shell/react";
import { memo, type ReactNode } from "react";
import AttachmentTypeIcon from "../components/AttachmentTypeIcon";
import Icon from "../components/Icons";
import { useAppStore } from "../store";
import { relativeTimeFrom } from "@rightkit/platform-ui";
import { colorFor, initialOf, threadSenders } from "../lib/mail";
import { splitHighlightSentinels } from "../lib/highlightSnippet";
import type { Thread } from "../lib/ipc";

function searchTerms(query: string): string[] {
  return query
    .split(/\s+/)
    .filter((token) => !token.includes(":"))
    .map((token) => token.replace(/["*()]/g, "").trim())
    .filter((token) => token.length > 0);
}

// Compile highlight regex once per query instead of on every row render.
const highlightRegexCache = new Map<string, { pattern: RegExp; terms: string[] } | null>();

function getHighlightPattern(query: string): { pattern: RegExp; terms: string[] } | null {
  if (highlightRegexCache.has(query)) return highlightRegexCache.get(query)!;
  const terms = searchTerms(query);
  if (terms.length === 0) {
    highlightRegexCache.set(query, null);
    return null;
  }
  const pattern = new RegExp( // nosemgrep: javascript.lang.security.audit.detect-non-literal-regexp.detect-non-literal-regexp
    `(${terms.map((term) => term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`,
    "ig",
  );
  const result = { pattern, terms };
  highlightRegexCache.set(query, result);
  return result;
}

function highlightText(text: string, query: string): ReactNode {
  if (!text) return text;
  const cached = getHighlightPattern(query);
  if (!cached) return text;
  const { pattern, terms } = cached;
  pattern.lastIndex = 0;
  const parts = text.split(pattern);
  return parts.map((part, i) =>
    terms.some(
      (term) =>
        part.localeCompare(term, undefined, { sensitivity: "accent" }) === 0,
    ) ? (
      <mark className="search-hit" key={i}>
        {part}
      </mark>
    ) : (
      part
    ),
  );
}

// L11 sends a server-ranked snippet with its own sentinel-delimited match
// spans (see src/lib/highlightSnippet.ts) — render THOSE spans directly
// instead of re-running the client-side term regex above. Never inserts raw
// HTML: the sentinel string is split into typed segments and matched spans
// get the same `.search-hit` <mark> styling as the client-side highlighter.
function highlightSnippetSegments(raw: string): ReactNode {
  const segments = splitHighlightSentinels(raw);
  return segments.map((seg, i) =>
    seg.matched ? (
      <mark className="search-hit" key={i}>
        {seg.text}
      </mark>
    ) : (
      seg.text
    ),
  );
}

export const ThreadRow = memo(function ThreadRow({
  t,
  focused,
  isSelected,
  labelNameOf,
  onOpenAttachment,
  openingAttachmentKey,
  query,
}: {
  t: Thread;
  focused: boolean;
  isSelected: boolean;
  labelNameOf: (id: string) => string;
  onOpenAttachment: (t: Thread, name: string) => void;
  openingAttachmentKey: string | null;
  query: string;
}) {
  const openThreadView = useAppStore((s) => s.openThreadView);
  const setCtx = useAppStore((s) => s.setCtx);
  const toggleSelect = useAppStore((s) => s.toggleSelect);
  const toggleStarRow = useAppStore((s) => s.toggleStarRow);
  const density = useAppStore((s) => s.density);
  const snippet = t.matching_message_excerpt || t.snippet;
  // L11's FTS snippet() carries its own match spans — prefer it over the
  // client-side regex highlighter when present (more accurate: it reflects
  // what the backend actually matched, e.g. stemmed/tokenized hits the
  // client regex can't see).
  const serverSnippet = t.matching_message_snippet;
  const hasAttachments = t.attachment_names.length > 0;
  const compact = density === "compact";
  return (
    <div
      className={[
        "thread group grid grid-cols-[54px_8px_24px_minmax(128px,205px)_minmax(0,1fr)_72px] gap-2 border-b border-mr-line relative isolate cursor-pointer transition-colors duration-100 hover:bg-[color-mix(in_srgb,var(--panel-2)_88%,var(--paper)_12%)] active:bg-[color-mix(in_srgb,var(--copper)_12%,transparent)]",
        t.unread && "unread",
        hasAttachments && "has-attachments",
        isSelected &&
          "selected bg-[color-mix(in_srgb,var(--copper)_14%,transparent)]",
        focused &&
          "focused bg-[color-mix(in_srgb,var(--panel-2)_85%,var(--paper)_15%)] shadow-[inset_3px_0_0_var(--copper)]",
        hasAttachments
          ? compact
            ? "min-h-[52px] py-[4px]"
            : "min-h-[56px] py-[5px]"
          : compact
            ? "min-h-[34px] py-[3px]"
            : "min-h-[40px] py-1",
        hasAttachments ? "items-start" : "items-center",
      ]
        .filter(Boolean)
        .join(" ")}
      draggable
      onClick={() => openThreadView(t)}
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData(
          "application/mailright-thread",
          JSON.stringify({ account_id: t.account_id, thread_id: t.thread_id }),
        );
        e.dataTransfer.setData("text/plain", t.subject || t.from_name);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        setCtx({ x: e.clientX, y: e.clientY, kind: "thread", t });
      }}
    >
      <Button variant="ghost" className="hit-overlay" aria-label={`Open ${t.subject || t.from_name}`} />
      {/* Checkbox + star share one full-height zone that absorbs clicks, so
          clicking near (but not exactly on) them never opens the thread. */}
      <span
        className={[
          "t-controls flex cursor-default items-center justify-between pr-1",
          hasAttachments ? "self-start" : "self-stretch",
        ]
          .filter(Boolean)
          .join(" ")}
        onClick={(e) => e.stopPropagation()}
      >
        <span onKeyDown={(e) => e.stopPropagation()} title="Select">
          <Checkbox label={<span className="sr-only">Select</span>} checked={isSelected} onChange={() => toggleSelect(t)} />
        </span>
        <IconButton label={t.starred ? "Unstar" : "Star"}
          className={[
            "t-star inline-flex cursor-pointer items-center justify-center border-0 bg-transparent p-0 text-mr-muted-2 opacity-[0.36] transition-[color,opacity] duration-100 hover:text-mr-muted hover:opacity-[0.72]",
            t.starred && "on text-[var(--star)] opacity-100",
          ]
            .filter(Boolean)
            .join(" ")}
          title={t.starred ? "Unstar" : "Star"}
          onClick={() => toggleStarRow(t)}
          onKeyDown={(e) => e.stopPropagation()}
        >
          <Icon name="star" size={15} filled={t.starred} />
        </IconButton>
      </span>
      <span
        className={[
          "acc-dot h-[9px] w-[9px] rounded-full opacity-[0.38] transition-[box-shadow,opacity] duration-100 group-hover:opacity-[0.66]",
          t.unread &&
            "opacity-[0.82] shadow-[0_0_0_2px_color-mix(in_srgb,var(--copper)_16%,transparent)]",
          hasAttachments && "self-start",
          compact && "scale-[0.85]",
        ]
          .filter(Boolean)
          .join(" ")}
        style={{ background: t.account_color }}
        title={t.account_name}
      />
      <span
        className={[
          "t-avatar inline-flex flex-none items-center justify-center rounded-full font-semibold uppercase text-white",
          compact ? "h-7 w-7 text-xs" : "h-[22px] w-[22px] text-[11px]",
          hasAttachments && "self-start",
        ]
          .filter(Boolean)
          .join(" ")}
        style={{ background: colorFor(t.from_name) }}
      >
        {initialOf(t.from_name)}
      </span>
      <span
        className={[
          "t-from flex min-w-0 items-center gap-1.5",
          compact ? "text-[12.5px]" : "text-[13px]",
          t.unread ? "font-semibold text-mr-paper" : "text-mr-muted",
          hasAttachments && "self-start",
        ]
          .filter(Boolean)
          .join(" ")}
      >
        <span className="t-from-name min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">
          {threadSenders(t.participants, t.from_name)}
        </span>
        {t.message_count > 1 && (
          <span className="t-count ml-1.5 inline-block flex-none rounded-lg border border-mr-line px-[5px] text-[11px] tabular-nums text-mr-muted-2">
            {t.message_count}
          </span>
        )}
      </span>
      <span
        className={[
          "t-body flex min-w-0 flex-col overflow-hidden",
          hasAttachments
            ? "self-start justify-start gap-[3px]"
            : "self-center justify-center",
        ]
          .filter(Boolean)
          .join(" ")}
      >
        <span className="t-summary flex min-w-0 items-baseline gap-1 overflow-hidden whitespace-nowrap">
          <span
            className={[
              "t-subject inline",
              compact ? "text-[12.5px]" : "text-[13px]",
              t.unread ? "font-semibold text-mr-paper" : "text-mr-muted",
            ]
              .filter(Boolean)
              .join(" ")}
          >
            {highlightText(t.subject, query)}
          </span>
          {t.labels.slice(0, 2).map((l) => {
            const n = labelNameOf(l);
            return n && n !== l ? (
              <span
                className="t-chip mr-1.5 inline whitespace-nowrap rounded-[10px] bg-[color-mix(in_srgb,var(--copper)_14%,transparent)] px-[7px] py-px align-middle text-[11px] text-mr-muted-2"
                key={l}
              >
                {n}
              </span>
            ) : null;
          })}
          {snippet && (
            <>
              <span className="t-sep flex-none text-mr-muted-2">-</span>
              <span
                className={[
                  "t-snippet min-w-0 overflow-hidden text-ellipsis text-mr-muted-2",
                  compact ? "text-[12.5px]" : "text-[13px]",
                ]
                  .filter(Boolean)
                  .join(" ")}
              >
                {serverSnippet ? highlightSnippetSegments(serverSnippet) : highlightText(snippet, query)}
                {query.trim() && (t.match_count ?? 0) > 1 && (
                  <span className="match-badge">{t.match_count} matches</span>
                )}
              </span>
            </>
          )}
        </span>
        {hasAttachments && (
          <span className="t-attachments flex min-w-0 items-center gap-1.5 overflow-hidden text-[12px] leading-[1.15] text-mr-muted-2">
            {t.attachment_names.slice(0, 2).map((name) => {
              const attachmentKey = `${t.account_id}:${t.thread_id}:${name}`;
              const opening = openingAttachmentKey === attachmentKey;
              return (
              <Button variant="ghost"
                type="button"
                className="t-att-item inline-flex max-w-[220px] min-w-0 items-center gap-[5px] overflow-hidden bg-transparent p-0 text-mr-muted-2"
                key={name}
                title={`Open ${name}`}
                aria-label={`Open attachment ${name}`}
                disabled={opening}
                onClick={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  onOpenAttachment(t, name);
                }}
                onKeyDown={(e) => e.stopPropagation()}
              >
                <AttachmentTypeIcon filename={name} size="sm" />
                <span className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">
                  {opening ? "Opening..." : name}
                </span>
              </Button>
              );
            })}
            {t.attachment_names.length > 2 && (
              <span className="t-att-more flex-none text-[11px] text-mr-muted-2">
                +{t.attachment_names.length - 2}
              </span>
            )}
          </span>
        )}
      </span>
      <span
        className={[
          "t-meta flex min-h-[22px] min-w-0 items-center justify-end gap-2 overflow-visible pr-1",
          hasAttachments && "self-start",
        ]
          .filter(Boolean)
          .join(" ")}
      >
        {t.has_attachments && t.attachment_names.length === 0 && (
          <Icon
            name="paperclip"
            size={14}
            className="t-attach mr-1 flex-none text-mr-muted-2"
          />
        )}
        <span
          className={[
            "t-time block min-w-[64px] whitespace-nowrap text-right font-mr-mono text-[11px]",
            t.unread ? "font-bold text-mr-paper" : "text-mr-muted-2",
          ]
            .filter(Boolean)
            .join(" ")}
        >
          {relativeTimeFrom(t.last_message_at)}
        </span>
      </span>
    </div>
  );
});
