// Find-in-page bar (ported from ReadRight, restyled for the host app). Read-only —
// searches the open conversation via the thread-wide SearchProvider. The
// provider returns the authoritative {count, activeIndex}; we never compute the
// active index here.
import { IconButton, Input } from "@rightkit/react-shell/react";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { useEffect, useRef, useState } from "react";
import type { SearchOptions, SearchProvider, SearchResult } from "../lib/searchUtils";

const EMPTY: SearchResult = { count: 0, activeIndex: -1 };

export function FindBar({
  provider,
  onClose,
}: {
  provider: SearchProvider | null;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [result, setResult] = useState<SearchResult>(EMPTY);
  const [matchCase, setMatchCase] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  // Escape closes the bar. useDismiss reads it in the window capture phase and stops it, so it
  // beats App's global Escape handler (which would otherwise close the thread/menu). Same
  // behaviour as the old local listener: no IME guard, no outside press.
  useDismiss(() => onClose(), { imeGuard: false, outside: false });

  // Run find on query/option change (debounced for non-empty queries).
  useEffect(() => {
    if (!provider) return;
    let live = true;
    const opts: SearchOptions = { matchCase, wholeWord };
    const run = () => {
      const res = provider.find(query, opts);
      if (live) setResult(query ? res : EMPTY);
    };
    if (!query) {
      run();
      return () => {
        live = false;
      };
    }
    const t = setTimeout(run, 150);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [query, provider, matchCase, wholeWord]);

  const step = (dir: "next" | "prev") => {
    if (!provider || result.count === 0) return;
    setResult(dir === "next" ? provider.next() : provider.prev());
  };

  return (
    <div className="findbar" role="search">
      <Input
        ref={inputRef}
        className="findbar-input"
        type="text"
        value={query}
        placeholder="Find in conversation…"
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            step(e.shiftKey ? "prev" : "next");
          }
        }}
      />
      <IconButton label="Match case" pressed={matchCase} onClick={() => setMatchCase((v) => !v)}>
        Aa
      </IconButton>
      <IconButton label="Whole word" pressed={wholeWord} onClick={() => setWholeWord((v) => !v)}>
        W
      </IconButton>
      <span className="findbar-count">
        {result.count > 0 ? `${result.activeIndex + 1}/${result.count}` : "0/0"}
      </span>
      <IconButton label="Previous (Shift+Enter)" disabled={result.count === 0} onClick={() => step("prev")}>
        ↑
      </IconButton>
      <IconButton label="Next (Enter)" disabled={result.count === 0} onClick={() => step("next")}>
        ↓
      </IconButton>
      <IconButton label="Close (Esc)" onClick={onClose}>
        ✕
      </IconButton>
    </div>
  );
}
