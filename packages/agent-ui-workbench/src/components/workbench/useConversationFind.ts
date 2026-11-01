import { useCallback, useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import type { PendingApproval } from "@rightkit/agent-ui-hooks/hooks/useApprovals";
import type { QueuedMessage } from "@rightkit/agent-ui-hooks/hooks/useQueuedMessages";
import type { TranscriptMessage, UserQuestion } from "@rightkit/agent-ui-core/lib/types";
import { MAX_TRANSCRIPT_SEARCH_MATCHES, searchTranscript } from "@rightkit/agent-ui-core/lib/transcriptSearch";
import type { Pane } from "./WorkbenchContentProps";
import { registerShortcut } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";

const FIND_HIGHLIGHT_STYLE_ID = "cr-find-highlight-style";

function ensureFindHighlightStyle() {
  if (typeof document === "undefined" || document.getElementById(FIND_HIGHLIGHT_STYLE_ID)) {
    return;
  }
  const style = document.createElement("style");
  style.id = FIND_HIGHLIGHT_STYLE_ID;
  style.textContent = `
::highlight(cr-find) {
	background-color: color-mix(in srgb, var(--color-warning) 38%, transparent);
	color: var(--color-text);
}
::highlight(cr-find-active) {
	background-color: var(--color-accent);
	color: var(--color-on-accent);
}
`;
  document.head.appendChild(style);
}

export function useConversationFind({
  pane,
  messages,
  streamingText,
  scrollRef,
}: {
  pane: Pane;
  messages: TranscriptMessage[];
  streamingText: string;
  streamingReasoning: string;
  pendingApproval: PendingApproval | null;
  pendingQuestion: { questionId: string; questions: UserQuestion[] } | null;
  pendingPlan: { planId: string; plan: string } | null;
  queuedMessages: QueuedMessage[];
  scrollRef: RefObject<HTMLDivElement | null>;
}) {
  // Find-in-conversation (Ctrl/Cmd+F). Uses the CSS Custom Highlight API so we
  // never mutate the React-owned DOM — matches are painted via ::highlight().
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findCount, setFindCount] = useState(0);
  const [findActive, setFindActive] = useState(0); // 1-based index of the focused match
  const findInputRef = useRef<HTMLInputElement>(null);
  const findRangesRef = useRef<Range[]>([]);
  const supportsHighlight =
    typeof CSS !== "undefined" && "highlights" in CSS && "Highlight" in window;

  useEffect(() => {
    if (supportsHighlight) ensureFindHighlightStyle();
  }, [supportsHighlight]);

  const clearFindHighlights = useCallback(() => {
    if (!supportsHighlight) return;
    const reg = (CSS as unknown as { highlights: Map<string, unknown> }).highlights;
    reg.delete("cr-find");
    reg.delete("cr-find-active");
  }, [supportsHighlight]);

  const closeFind = useCallback(() => {
    setFindOpen(false);
    setFindQuery("");
    setFindCount(0);
    setFindActive(0);
    findRangesRef.current = [];
    clearFindHighlights();
  }, [clearFindHighlights]);
  useEffect(() => {
    if (pane !== "session") return;
    return registerShortcut({
      id: "conversation.find",
      // Old matching: Ctrl or Meta (either, or both), no Shift, no Alt; no composing or defaultPrevented
      // guard. `ignoreExtraModifiers` admits both-held; the run guard restores the Shift/Alt exclusion
      // and reports the key unhandled (no preventDefault), as before.
      chord: "ctrlormeta+f",
      label: "Find in conversation",
      hidden: true,
      ignoreExtraModifiers: true,
      allowComposing: true,
      allowDefaultPrevented: true,
      run: (event) => {
        if (event.shiftKey || event.altKey) return false;
        setFindOpen(true);
        requestAnimationFrame(() => findInputRef.current?.select());
        return true;
      },
    });
  }, [pane]);
  // Escape closes the find bar. It stays a window listener on purpose: Escape has several owners that
  // all react to the same press, and the shortcut registry runs only the first matching binding.
  useEffect(() => {
    if (pane !== "session" || !findOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeFind();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [pane, findOpen, closeFind]);

  // Recompute match ranges when the query or transcript changes.
  // biome-ignore lint/correctness/useExhaustiveDependencies: ranges follow transcript content, not every prop.
  useEffect(() => {
    if (!findOpen || !supportsHighlight) return;
    const root = scrollRef.current;
    const q = findQuery.trim().toLowerCase();
    findRangesRef.current = [];
    if (!root || q.length === 0) {
      setFindCount(0);
      setFindActive(0);
      return;
    }
    const ranges: Range[] = [];
    const indexed = searchTranscript(messages, q, { maxMatches: MAX_TRANSCRIPT_SEARCH_MATCHES });
    if (indexed.matches.length === 0) {
      setFindCount(0);
      setFindActive(0);
      return;
    }
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node && ranges.length < indexed.matches.length) {
      const text = node.nodeValue ?? "";
      const lower = text.toLowerCase();
      let idx = lower.indexOf(q);
      while (idx !== -1 && ranges.length < indexed.matches.length) {
        const r = document.createRange();
        r.setStart(node, idx);
        r.setEnd(node, idx + q.length);
        ranges.push(r);
        idx = lower.indexOf(q, idx + q.length);
      }
      node = walker.nextNode();
    }
    findRangesRef.current = ranges;
    setFindCount(indexed.matches.length);
    setFindActive(indexed.matches.length ? 1 : 0);
  }, [findOpen, findQuery, messages, streamingText, supportsHighlight]);

  // Paint the highlights and scroll the active match into view.
  useEffect(() => {
    if (!supportsHighlight) return;
    const reg = (CSS as unknown as { highlights: Map<string, unknown> }).highlights;
    const HighlightCtor = (window as unknown as { Highlight: new (...r: Range[]) => unknown })
      .Highlight;
    reg.delete("cr-find");
    reg.delete("cr-find-active");
    const ranges = findRangesRef.current;
    if (!findOpen || ranges.length === 0) return;
    const activeIdx = findActive - 1;
    const rest = ranges.filter((_, i) => i !== activeIdx);
    if (rest.length) reg.set("cr-find", new HighlightCtor(...rest));
    const active = ranges[activeIdx];
    if (active) {
      reg.set("cr-find-active", new HighlightCtor(active));
      const root = scrollRef.current;
      const rect = active.getBoundingClientRect();
      if (root) {
        const box = root.getBoundingClientRect();
        if (rect.top < box.top + 48 || rect.bottom > box.bottom - 48) {
          root.scrollTop += rect.top - box.top - root.clientHeight / 2;
        }
      }
    }
  }, [findActive, findOpen, supportsHighlight, scrollRef.current]);

  useEffect(() => () => clearFindHighlights(), [clearFindHighlights]);

  // Leaving the session pane (component stays mounted) must not leave stale
  // highlights registered against a transcript that's no longer painted.
  useEffect(() => {
    if (pane !== "session" && findOpen) closeFind();
  }, [pane, findOpen, closeFind]);

  const stepFind = useCallback(
    (dir: 1 | -1) => {
      setFindActive((cur) => {
        if (findCount === 0) return 0;
        let next = cur + dir;
        if (next < 1) next = findCount;
        if (next > findCount) next = 1;
        return next;
      });
    },
    [findCount],
  );

  return {
    findOpen,
    findQuery,
    setFindQuery,
    findCount,
    findActive,
    findInputRef,
    supportsHighlight,
    closeFind,
    stepFind,
  };
}
