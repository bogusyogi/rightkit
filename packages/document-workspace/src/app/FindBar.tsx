import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { CaretDown } from "@phosphor-icons/react/dist/icons/CaretDown";
import { CaretUp } from "@phosphor-icons/react/dist/icons/CaretUp";
import { MagnifyingGlass } from "@phosphor-icons/react/dist/icons/MagnifyingGlass";
import { Swap } from "@phosphor-icons/react/dist/icons/Swap";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { createShortcutRegistry } from "@rightkit/react-shell";
import { isValidSearchRegex } from "@rightkit/document-core/documents/searchUtils";
import type { SearchOptions, SearchProvider, SearchResult } from "@rightkit/document-core/documents/types";
import { documentEvent } from "@rightkit/document-core/host";

interface FindBarProps {
  provider: SearchProvider | null;
  onClose: () => void;
  initialReplaceMode?: boolean;
  scope?: "current" | "all";
  onScopeChange?: (scope: "current" | "all") => void;
  canCrossDoc?: boolean;
  placement?: "floating" | "toolbar";
}

const EMPTY: SearchResult = { count: 0, activeIndex: -1 };

// Session-only recent-query history (no localStorage — nothing typed into find outlives the session).
const findHistory: string[] = [];
function rememberQuery(query: string) {
  const value = query.trim();
  if (!value) return;
  const at = findHistory.indexOf(value);
  if (at >= 0) findHistory.splice(at, 1);
  findHistory.unshift(value);
  if (findHistory.length > 12) findHistory.length = 12;
}

function PrevIcon() {
  return <CaretUp size={14} weight="regular" aria-hidden="true" />;
}

function NextIcon() {
  return <CaretDown size={14} weight="regular" aria-hidden="true" />;
}

export function FindBar({
  provider,
  onClose,
  initialReplaceMode = false,
  scope = "current",
  onScopeChange,
  canCrossDoc = false,
  placement = "floating",
}: FindBarProps) {
  const [query, setQuery] = useState("");
  const [replaceText, setReplaceText] = useState("");
  const [result, setResult] = useState<SearchResult>(EMPTY); // provider-owned count + activeIndex
  const [showReplace, setShowReplace] = useState(initialReplaceMode);
  const [matchCase, setMatchCase] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const [regex, setRegex] = useState(false);

  const queryInputRef = useRef<HTMLInputElement>(null);
  const replaceInputRef = useRef<HTMLInputElement>(null);

  const canReplace = !!provider?.canReplace;
  const showCaseToggle = !!provider?.supportsMatchCase;
  const showWordToggle = !!provider?.supportsWholeWord;
  const showRegexToggle = !!provider?.supportsRegex;
  const regexActive = regex && showRegexToggle;
  const regexInvalid = regexActive && query !== "" && !isValidSearchRegex(query);

  // Focus + select the query field the moment the bar opens, so keystrokes after Ctrl+F land in the
  // search box, not the document body. useLayoutEffect (not useEffect) runs synchronously before
  // paint — ahead of editors that re-grab focus via requestAnimationFrame on mount (the CodeMirror
  // source editor's autofocus, ProseMirror). The rAF re-assert reclaims focus if such a competitor's
  // callback still fires after us; it only acts when focus actually drifted off the input.
  useLayoutEffect(() => {
    const input = queryInputRef.current;
    if (!input) return;
    input.focus();
    input.select();
    const raf = requestAnimationFrame(() => {
      if (queryInputRef.current === input && document.activeElement !== input) {
        input.focus();
        input.select();
      }
    });
    return () => cancelAnimationFrame(raf);
  }, []);

  useEffect(() => {
    setShowReplace(initialReplaceMode && !!provider?.canReplace);
  }, [initialReplaceMode, provider]);

  // Escape closes the bar. Bubble phase on the window, like the handler it replaces, so a modal
  // that takes Escape in the capture phase (the Link dialog) keeps Find open beneath it.
  const windowTarget = useRef<EventTarget | null>(typeof window === "undefined" ? null : window);
  useDismiss(() => onClose(), {
    escapeTarget: windowTarget,
    outside: false,
    preventDefault: true,
    stopPropagation: false,
    imeGuard: false,
  });

  // Alt+R toggles regex through a registry scoped to this bar: registered while it is open,
  // dispatching in the bubble phase on the window like the matcher it replaces.
  const regexToggleAvailableRef = useRef(showRegexToggle);
  regexToggleAvailableRef.current = showRegexToggle;
  useEffect(() => {
    const registry = createShortcutRegistry();
    registry.register({
      id: "find.toggle-regex",
      label: "Toggle regular expression",
      chord: "alt+r",
      hidden: true,
      when: () => regexToggleAvailableRef.current,
      allowDefaultPrevented: true,
      allowComposing: true,
      run: () => setRegex((v) => !v),
    });
    return registry.attach(window);
  }, []);

  // Run find when the query or the options change. Provider returns the authoritative
  // {count, activeIndex}; we never compute the active index locally. Non-empty queries are
  // debounced so large docs (and PDF page extraction / DOCX scans) don't run on every keystroke.
  useEffect(() => {
    if (!provider) return;
    let current = true;
    const opts: SearchOptions = { matchCase, wholeWord, regex: regexActive };
    const run = async () => {
      // An invalid pattern matches nothing: clear provider highlights via an empty query.
      const res =
        regexActive && query && !isValidSearchRegex(query)
          ? await provider.find("", opts)
          : await provider.find(query, opts);
      if (current) setResult(query ? res : EMPTY);
    };
    if (!query) {
      void run();
      return () => {
        current = false;
      };
    }
    const t = setTimeout(run, 180);
    return () => {
      current = false;
      clearTimeout(t);
    };
  }, [query, provider, matchCase, wholeWord, regexActive]);

  // Providers can change their match set without a FindBar action (document undo/redo, external
  // reconciliation). Keep displayed provider-owned count/index synchronized with those changes.
  useEffect(() => {
    if (!provider?.subscribe) return;
    return provider.subscribe((providerQuery, next) => {
      if (providerQuery === query) setResult(query ? next : EMPTY);
    });
  }, [provider, query]);

  const step = async (dir: "next" | "prev") => {
    if (!provider || result.count === 0) return;
    setResult(await (dir === "next" ? provider.next() : provider.prev()));
  };

  // F3 / Shift+F3 (dispatched by the global shortcut when the bar is already open) step the match
  // through the bar itself, so its displayed count/index stays the source of truth. The listener is
  // registered ONCE and reads the latest `step` via a ref (avoids re-subscribing every render).
  const stepRef = useRef(step);
  stepRef.current = step;
  useEffect(() => {
    const onStep = (e: Event) => {
      const dir = (e as CustomEvent<{ dir: number }>).detail?.dir;
      void stepRef.current(dir && dir < 0 ? "prev" : "next");
    };
    window.addEventListener(documentEvent("find-step"), onStep);
    return () => window.removeEventListener(documentEvent("find-step"), onStep);
  }, []);

  const doReplace = async () => {
    if (!provider?.replace) return;
    setResult(await provider.replace(replaceText));
  };
  const doReplaceAll = async () => {
    if (!provider?.replaceAll) return;
    setResult(await provider.replaceAll(replaceText));
  };

  const resultStatus =
    regexInvalid
      ? "Invalid regular expression"
      : result.count > 0
      ? `Match ${result.activeIndex + 1} of ${result.count}`
      : query.trim()
        ? "No matches"
        : "No search results";

  const handleQueryKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      rememberQuery(query);
      void step(e.shiftKey ? "prev" : "next");
    }
  };
  const handleReplaceKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      void doReplace();
    }
  };

  // Shared inner field: rounded capsule on the recessed ink surface, copper hairline while any
  // control inside is focused (the inputs themselves stay outline-free).
  const fieldCls =
    "flex h-8 min-w-[120px] flex-1 basis-[160px] items-center gap-2 rounded-full border border-transparent bg-ink/70 pl-3 pr-2 transition-colors focus-within:border-copper/60";
  const inputCls =
    "min-w-0 flex-1 bg-transparent text-chrome-md text-paper outline-none placeholder:text-muted-2";
  const iconBtnCls = (on: boolean, disabled = false) =>
    `vr-press vr-focus flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-chrome-xs ${
      on
        ? "bg-copper/[0.14] text-copper-hi"
        : disabled
          ? "text-vr-disabled"
          : "text-muted hover:bg-ink/40 hover:text-paper"
    }`;
  const pillBtnCls =
    "vr-press vr-focus flex h-8 shrink-0 items-center justify-center rounded-full px-3 text-chrome-sm font-medium";

  return (
    <div
      data-vr-find-placement={placement}
      role="search"
      aria-label="Find in document"
      className="no-print relative flex w-full max-w-full flex-col gap-2 font-sans text-chrome-md select-none"
    >
      <div className="vr-capsule min-h-11 flex-wrap gap-1 px-1.5 py-1">
        <div className={`${fieldCls}${regexInvalid ? " !border-red-500/70" : ""}`}>
          <MagnifyingGlass
            size={14}
            weight="regular"
            aria-hidden="true"
            className="shrink-0 text-muted-2"
          />
          <input
            ref={queryInputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleQueryKeyDown}
            aria-label="Find text"
            aria-invalid={regexInvalid}
            placeholder="Find..."
            list="vr-find-history"
            className={inputCls}
          />
          <datalist id="vr-find-history">
            {findHistory.map((q) => (
              <option key={q} value={q} />
            ))}
          </datalist>
          <span className="shrink-0 whitespace-nowrap pl-1 text-chrome-xs text-muted tabular-nums">
            {regexInvalid
              ? "Invalid"
              : result.count > 0
                ? `${result.activeIndex + 1} / ${result.count}`
                : "0 / 0"}
          </span>
        </div>
        {(showCaseToggle || showWordToggle || showRegexToggle) && (
          <div className="flex shrink-0 items-center gap-0.5">
            {showCaseToggle && (
              <button
                type="button"
                onClick={() => setMatchCase((v) => !v)}
                aria-label="Match case"
                aria-pressed={matchCase}
                className={iconBtnCls(matchCase)}
                title="Match case"
              >
                Aa
              </button>
            )}
            {showWordToggle && (
              <button
                type="button"
                onClick={() => setWholeWord((v) => !v)}
                aria-label="Whole word"
                aria-pressed={wholeWord}
                className={iconBtnCls(wholeWord)}
                title="Whole word"
              >
                W
              </button>
            )}
            {showRegexToggle && (
              <button
                type="button"
                onClick={() => setRegex((v) => !v)}
                aria-label="Regex"
                aria-pressed={regex}
                aria-keyshortcuts="Alt+R"
                className={iconBtnCls(regex)}
                title="Regular expression (Alt+R)"
              >
                .*
              </button>
            )}
          </div>
        )}
        {canCrossDoc && onScopeChange && (
          <button
            type="button"
            onClick={() => onScopeChange(scope === "current" ? "all" : "current")}
            aria-label={scope === "all" ? "Searching all tabs" : "Search this tab only"}
            aria-pressed={scope === "all"}
            className={`${iconBtnCls(scope === "all")} w-auto px-2 text-chrome-xs`}
            title={
              scope === "all"
                ? "Searching all tabs — click to limit to this tab"
                : "Search this tab only — click to search all tabs"
            }
          >
            {scope === "all" ? "all" : "1"}
          </button>
        )}
        <button
          type="button"
          onClick={() => void step("prev")}
          disabled={result.count === 0}
          aria-label="Previous match"
          title="Previous match"
          className={iconBtnCls(false, result.count === 0)}
        >
          <PrevIcon />
        </button>
        <button
          type="button"
          onClick={() => void step("next")}
          disabled={result.count === 0}
          aria-label="Next match"
          title="Next match"
          className={iconBtnCls(false, result.count === 0)}
        >
          <NextIcon />
        </button>
        {canReplace && (
          <button
            type="button"
            onClick={() => setShowReplace((v) => !v)}
            aria-pressed={showReplace}
            aria-expanded={showReplace}
            title={showReplace ? "Hide replace field" : "Show replace field"}
            className={`${pillBtnCls} ${showReplace ? "bg-copper/[0.14] text-copper-hi" : "text-muted hover:bg-ink/40 hover:text-paper"}`}
          >
            Replace
          </button>
        )}
        <button
          type="button"
          onClick={onClose}
          title="Close find"
          className={`${pillBtnCls} text-paper/85 hover:bg-ink/40 hover:text-paper`}
        >
          Done
        </button>
      </div>
      {showReplace && canReplace && (
        <div
          data-vr-find-replace-row=""
          className="vr-capsule min-h-11 flex-wrap gap-1 px-1.5 py-1"
        >
          <div className={fieldCls}>
            <Swap size={14} weight="regular" aria-hidden="true" className="shrink-0 text-muted-2" />
            <input
              ref={replaceInputRef}
              type="text"
              value={replaceText}
              onChange={(e) => setReplaceText(e.target.value)}
              onKeyDown={handleReplaceKeyDown}
              aria-label="Replace with"
              placeholder="Replace with..."
              className={inputCls}
            />
          </div>
          <button
            type="button"
            onClick={() => void doReplace()}
            disabled={result.count === 0}
            title="Replace current match"
            className={`${pillBtnCls} text-muted hover:bg-ink/40 hover:text-paper disabled:text-vr-disabled disabled:hover:bg-transparent`}
          >
            Replace
          </button>
          <button
            type="button"
            onClick={() => void doReplaceAll()}
            disabled={result.count === 0}
            title="Replace all matches"
            className={`${pillBtnCls} bg-copper text-white hover:bg-copper/90 disabled:bg-panel-2 disabled:text-vr-disabled`}
          >
            All
          </button>
        </div>
      )}
      <span className="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {resultStatus}
      </span>
    </div>
  );
}
