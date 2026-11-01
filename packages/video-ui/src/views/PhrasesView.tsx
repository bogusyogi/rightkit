// Find phrases — docs/design/app-ui.md §3.6.
import { Badge, Button, EmptyState, Field, IconButton, Input, PageShell, SearchIcon, useShortcuts, useToast } from "@rightkit/react-shell/react";
import "./Phrases.css";
import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../lib/engine";
import { clipExport, type PhraseMatch, phraseSearch, sourceList } from "../lib/media";
import { appendClip } from "../lib/project";
import { RangePreview } from "../components/RangePreview";
import { ErrorPanel, timecode, Icon } from "../ui";

type SourceRow = { id: string; name: string; path: string; exists: boolean; has_transcript: boolean };

/** Inline preview popover: plays the match's context range on the real file. */
function PreviewPop({ path, startMs, endMs, onClose }: { path: string; startMs: number; endMs: number; onClose: () => void }) {
  return (
    <div className="phrases-pop">
      <div className="phrases-pop__head">
        <span className="cr-overline">Preview</span>
        <IconButton label="Close preview" onClick={onClose}><Icon name="x" /></IconButton>
      </div>
      <RangePreview path={path} startMs={startMs} endMs={endMs} />
    </div>
  );
}

/** Highlight the query inside the context sentence (`--accent-soft` mark). */
function Context({ text, query }: { text: string; query: string }) {
  const i = text.toLowerCase().indexOf(query.toLowerCase());
  if (i < 0 || !query.trim()) return <span>{text}</span>;
  return (
    <span>
      {text.slice(0, i)}
      <mark className="phrases-mark">{text.slice(i, i + query.length)}</mark>
      {text.slice(i + query.length)}
    </span>
  );
}

export function PhrasesView({ dir, onChanged }: { dir: string; onChanged: () => Promise<void> }) {
  const toast = useToast();
  const [sources, setSources] = useState<SourceRow[]>([]);
  const [filter, setFilter] = useState("");
  const [query, setQuery] = useState("");
  const [matches, setMatches] = useState<PhraseMatch[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<PhraseMatch | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    sourceList(dir)
      .then((s) => setSources(s.sources as SourceRow[]))
      .catch((e) => setError(errorMessage(e)));
  }, [dir]);

  // ⌘F focuses the search field.
  useShortcuts([
    {
      id: "cr.phrases.search",
      chord: "mod+f",
      label: "Search phrases",
      group: "Find phrases",
      run: () => {
        inputRef.current?.focus();
        inputRef.current?.select();
      },
    },
  ]);

  const search = useCallback(async () => {
    if (!query.trim()) return;
    setBusy("search");
    setError(null);
    try {
      const r = await phraseSearch(dir, query.trim());
      setMatches(r.matches);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  }, [dir, query]);

  const insert = async (m: PhraseMatch) => {
    setBusy(`${m.source}:${m.word_index}`);
    try {
      await appendClip(dir, m.source, m.context.start_ms, m.context.end_ms);
      toast("Added to the timeline", { kind: "success" });
      await onChanged();
    } catch (e) {
      toast(errorMessage(e), { kind: "error", ttl: 0 });
    } finally {
      setBusy(null);
    }
  };

  const exportClip = async (m: PhraseMatch) => {
    setBusy(`export-${m.source}:${m.word_index}`);
    try {
      const r = await clipExport(dir, {
        source: m.source,
        start_ms: m.context.start_ms,
        end_ms: m.context.end_ms,
      });
      toast(`Exported ${r.name} — find it under Exports`, { kind: "success" });
      await onChanged();
    } catch (e) {
      toast(errorMessage(e), { kind: "error", ttl: 0 });
    } finally {
      setBusy(null);
    }
  };

  const srcById = new Map(sources.map((s) => [s.id, s]));
  const shown = (matches ?? []).filter((m) => !filter || m.source === filter);

  return (
    <PageShell title="Find phrases" maxWidth={0} subtitle={"Search spoken words across this project's transcripts"}>
      <div className="phrases-body">
        <div className="phrases-controls">
          <Field label="Search" note={"Local search — indexes your own footage only."}>{(ids) => (<Input {...ids}
              ref={inputRef}

              placeholder="Find spoken words…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void search()}
            />)}</Field>
          <Field label="Source">{(ids) => (<select {...ids} className="rk-input cr-select" value={filter} onChange={(e) => setFilter(e.target.value)}>
              <option value="">All sources</option>
              {sources.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>)}</Field>
          <Button
            variant="primary"
            onClick={() => void search()}
            disabled={(!query.trim()) || (busy === "search")}
            aria-busy={(busy === "search") || undefined}
          >{!(busy === "search") && <span className="cr-glyph"><SearchIcon /></span>}
            {busy === "search" ? "Searching…" : "Search"}
          </Button>
        </div>

        {error && <ErrorPanel title="Search failed" detail={error} />}

        {matches !== null && shown.length === 0 && (
          <EmptyState title="No matches" icon={<span className="cr-glyph cr-glyph--lg"><SearchIcon /></span>}>
            Nothing in this project's transcripts matches that phrase.
          </EmptyState>
        )}
        {matches === null && !error && (
          <EmptyState title="Find what's said" icon={<Icon name="text-quote" width={28} height={28} />}>
            Search every transcript for a phrase, then preview or drop it on the timeline.
          </EmptyState>
        )}

        <div className="phrases-results">
          {shown.map((m, i) => {
            const src = srcById.get(m.source);
            const key = `${m.source}:${m.word_index}:${i}`;
            return (
              <div key={key} className="phrases-row">
                <div className="phrases-row__top">
                  <span className="cr-mono cr-muted">{timecode(m.start_ms)}</span>
                  <Badge>{src?.name ?? m.source}</Badge>
                  <span className="phrases-row__spacer" />
                  {src?.path ? (
                    <Button size="sm" variant="ghost" onClick={() => setPreview(preview === m ? null : m)}>
                      Preview
                    </Button>
                  ) : null}
                  <Button
                    size="sm"
                    variant="ghost"
                    title="Render this phrase range to Exports"
                    onClick={() => void exportClip(m)}
                    disabled={(busy === `export-${m.source}:${m.word_index}`)}
                    aria-busy={(busy === `export-${m.source}:${m.word_index}`) || undefined}
                  >
                    {busy === `export-${m.source}:${m.word_index}` ? "Exporting…" : "Export clip"}
                  </Button>
                  <Button
                    size="sm"
                    variant="secondary"
                    onClick={() => void insert(m)}
                    disabled={(busy === `${m.source}:${m.word_index}`)}
                    aria-busy={(busy === `${m.source}:${m.word_index}`) || undefined}
                  >
                    {busy === `${m.source}:${m.word_index}` ? "Adding…" : "Add to timeline"}
                  </Button>
                </div>
                <p className="phrases-row__ctx cr-muted">
                  …<Context text={m.context.text} query={query} />…
                </p>
                {preview === m && src?.path && (
                  <PreviewPop path={src.path} startMs={m.context.start_ms} endMs={m.context.end_ms} onClose={() => setPreview(null)} />
                )}
              </div>
            );
          })}
        </div>
      </div>
    </PageShell>
  );
}
