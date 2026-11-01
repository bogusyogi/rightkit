import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import { approveDraft, assetUrl, deleteAsset, errorMessage, errorText, fetchEngineSettings, fetchGallery, searchLibrary, studio } from '../lib/api';
import { useApp } from '../lib/app-state';
import type { GalleryArtifact, GalleryReport } from '../lib/types';
import type { AssetFacts, AssetScope, LibraryHit } from '../lib/types/library';
import { Badge, Button, Card, Checkbox, EmptyState, Input, Notice, Pill, SegmentedControl, Select } from '@rightkit/react-shell/react';
import { Kv, baseName, relativeTime, usd } from '../components/ui';
import { MediaPlayer } from '@rightkit/media-ui';
import { ImageViewer } from '@rightkit/media-ui/image';
import { useConfirm } from '../lib/confirm';
import { formatDuration } from '../lib/gen-params';
import brandCatalog from '../studio-data/brands/brands.json';
import {
  ALL,
  EYES_STATES,
  JURY_STATES,
  METADATA_STATES,
  NO_FILTERS,
  NO_PROJECT_FILTER,
  activeFilterCount,
  brandLabel,
  callerLabel,
  humanize,
  matchesFilters,
  optionSourceItems,
  optionsFor,
  projectLabel,
  serverFilters,
  stateLabel,
  stateTone,
  type BrandInfo,
  type FilterFields,
  type GalleryFilters
} from '../lib/gallery-filters';
import { NOT_RECORDED, factsRows } from '../lib/asset-facts';
import { LibraryResults, ImportPanel } from './GalleryLibrary';
import './gallery.css';
import { hostAppName } from "../appName";

/** Artifacts fetched per page; the engine walks every root and returns newest first. */
const GALLERY_PAGE = 200;
/** Library hits per page: the engine's own default for library.search. */
const LIBRARY_PAGE = 20;

const BRANDS: BrandInfo[] = brandCatalog.brands;
const BRAND_CODES = BRANDS.map((b) => b.code);

/** Library kinds shown as video or audio in the inspector; every other kind is an image. */
const VIDEO_KINDS = new Set(['video', 'lipsync', 'avatar', 'extend', 'motion_control', 'replace_object', 'restyle']);
const AUDIO_KINDS = new Set(['voice', 'music', 'sfx']);

/** Adds the page's artifacts that the list does not already hold, keyed by artifact id. */
function appendNew(prev: GalleryArtifact[], incoming: GalleryArtifact[]): GalleryArtifact[] {
  return [...prev, ...incoming.filter((a) => !prev.some((p) => p.artifact_id === a.artifact_id))];
}

interface Item extends FilterFields {
  id: string;
  /** How the file's caller named itself ("the host app"); empty when none registered it. */
  callerLabel: string;
  name: string;
  sub: string;
  url: string;
  rows: Array<[string, string]>;
  path: string;
  prompt: string | null;
  artifact: GalleryArtifact | null;
  /** Origin, evidence and rights as the engine recorded them; null when the engine holds none for this file. */
  facts: AssetFacts | null;
  scope: AssetScope | null;
  /** False when the library record is known but the file is gone. */
  exists?: boolean;
}

function fromArtifact(a: GalleryArtifact): Item {
  const type = a.artifact_type === 'video' ? 'video' : a.artifact_type === 'audio' ? 'audio' : 'image';
  const name = baseName(a.absolute_path, a.artifact_id);
  const sub = `${projectLabel(a.project_slug)} · ${relativeTime(a.indexed_at)}`;
  return {
    id: a.artifact_id,
    name,
    sub,
    url: assetUrl(a.absolute_path),
    type,
    path: a.absolute_path,
    project: a.project_slug,
    brand: a.brand,
    eyes: a.approval.eyes,
    jury: a.approval.auto_jury,
    metadata: a.metadata_status,
    caller: a.caller ?? '',
    callerLabel: a.caller_label ?? a.caller ?? '',
    haystack: `${name} ${sub} ${a.brand} ${a.generation.model ?? ''} ${a.generation.provider ?? ''} ${a.caller_label ?? a.caller ?? ''}`.toLowerCase(),
    prompt: a.generation.prompt,
    artifact: a,
    facts: a.asset_facts ?? null,
    scope: a.scope ?? null,
    rows: [
      ['Project', projectLabel(a.project_slug)],
      ['Brand', brandLabel(a.brand, BRANDS)],
      ...(a.caller ? [['Registered by', a.caller_label ?? a.caller] as [string, string]] : []),
      ['Provider', a.generation.provider ?? 'unknown'],
      ['Model', a.generation.model ?? 'unknown'],
      ['Aspect', a.generation.aspect ?? 'unknown'],
      ['Tool version', a.generation.chat_tool_version ?? 'unknown'],
      ['Metadata', humanize(a.metadata_status)],
      ['Cost', a.generation.cost_usd == null ? 'unknown' : usd(a.generation.cost_usd)]
    ]
  };
}

/** A library search hit, shown in the inspector. Hits carry no brand, review or jury state. */
function fromHit(h: LibraryHit): Item {
  const name = baseName(h.path, h.asset_id);
  const type = VIDEO_KINDS.has(h.kind) ? 'video' : AUDIO_KINDS.has(h.kind) ? 'audio' : 'image';
  return {
    id: `lib:${h.asset_id}`,
    name,
    sub: `library · ${h.kind}`,
    url: assetUrl(h.path),
    type,
    path: h.path,
    project: '',
    brand: 'NA',
    eyes: 'unknown',
    jury: 'unknown',
    metadata: 'complete',
    caller: '',
    callerLabel: '',
    haystack: name.toLowerCase(),
    prompt: null,
    artifact: null,
    facts: h.facts,
    scope: h.scope,
    exists: h.exists,
    rows: [
      ['Kind', h.kind],
      ['Provider', h.provider ?? NOT_RECORDED],
      ['Model', h.model ?? NOT_RECORDED],
      ['Asset id', h.asset_id],
      ['Job', h.job_id]
    ]
  };
}

/** Whether the engine narrows the listing for this filter state (project, brand, eyes or jury). */
function isServerFiltered(filters: ReturnType<typeof serverFilters>): boolean {
  return Object.keys(filters).length > 0;
}

function Filter({ label, value, options, onChange }: { label: string; value: string; options: Array<{ value: string; label: string; count: number }>; onChange: (v: string) => void }) {
  return (
    <label className="gf">
      {label}
      <Select
        label={`${label} filter`}
        value={value}
        onChange={onChange}
        options={[{ value: ALL, label: 'All' }, ...options.map((o) => ({ value: o.value, label: `${o.label} (${o.count})` }))]}
      />
    </label>
  );
}

function StateChip({ label, state }: { label: string; state: string }) {
  return <Badge tone={stateTone(state)}>{label}: {stateLabel(state)}</Badge>;
}

export function Gallery() {
  const app = useApp();
  const [artifacts, setArtifacts] = useState<GalleryArtifact[]>([]);
  // The listing from the most recent load with no server-side filter; dropdown options come from it while a filter is active.
  const [unfiltered, setUnfiltered] = useState<GalleryArtifact[]>([]);
  const [report, setReport] = useState<GalleryReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [filters, setFilters] = useState<GalleryFilters>(NO_FILTERS);
  const [includeIntermediate, setIncludeIntermediate] = useState(false);
  const [reloads, setReloads] = useState(0);
  const [eyesBusy, setEyesBusy] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  // Paths the engine removed this session. Generated outputs live in session state, so they are hidden here.
  const [removedPaths, setRemovedPaths] = useState<Set<string>>(() => new Set());
  const confirmAction = useConfirm();

  // Library search: the box text, the query whose results are showing (null means the normal listing), and its hits.
  const [libraryInput, setLibraryInput] = useState('');
  const [libraryQuery, setLibraryQuery] = useState<string | null>(null);
  const [libraryRun, setLibraryRun] = useState(0);
  const [libraryHits, setLibraryHits] = useState<LibraryHit[]>([]);
  const [libraryTotal, setLibraryTotal] = useState(0);
  const [libraryCursor, setLibraryCursor] = useState<string | null>(null);
  const [libraryNotIndexed, setLibraryNotIndexed] = useState<string[]>([]);
  const [libraryRebuilt, setLibraryRebuilt] = useState(false);
  const [libraryBusy, setLibraryBusy] = useState(false);
  const [libraryMoreBusy, setLibraryMoreBusy] = useState(false);
  const [libraryError, setLibraryError] = useState<string | null>(null);
  const [importOpen, setImportOpen] = useState(false);
  // Bumped by every new search or exit, so a response from an older query is dropped.
  const searchSeq = useRef(0);

  const set = useCallback(<K extends keyof GalleryFilters>(key: K, value: GalleryFilters[K]) => setFilters((f) => ({ ...f, [key]: value })), []);

  // Project, brand, eyes and jury are applied by the engine before paging. The search box, type and
  // metadata filters still apply to the loaded page. The memo keeps one identity until those four change.
  const { project, brand, eyes, jury, caller } = filters;
  const remote = useMemo(() => serverFilters({ ...NO_FILTERS, project, brand, eyes, jury, caller }), [project, brand, eyes, jury, caller]);
  // The filter the view is showing now, so a Load more that resolves after a filter change is dropped.
  const remoteRef = useRef(remote);
  useEffect(() => {
    remoteRef.current = remote;
  }, [remote]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        const settings = await fetchEngineSettings().catch(() => ({ gallery_roots: [] as string[] }));
        const res = await fetchGallery({ roots: settings.gallery_roots ?? [], offset: 0, limit: GALLERY_PAGE, includeIntermediate, ...remote });
        if (!cancelled) {
          setArtifacts(res.artifacts);
          if (!isServerFiltered(remote)) setUnfiltered(res.artifacts);
          setReport(res.report);
          setError(null);
        }
      } catch (e) {
        if (!cancelled) setError(errorMessage(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [includeIntermediate, reloads, remote]);

  const loadMore = useCallback(async () => {
    const requested = remote;
    setLoadingMore(true);
    try {
      const settings = await fetchEngineSettings().catch(() => ({ gallery_roots: [] as string[] }));
      const res = await fetchGallery({ roots: settings.gallery_roots ?? [], offset: artifacts.length, limit: GALLERY_PAGE, includeIntermediate, ...requested });
      if (remoteRef.current !== requested) return;
      setArtifacts((prev) => appendNew(prev, res.artifacts));
      if (!isServerFiltered(requested)) setUnfiltered((prev) => appendNew(prev, res.artifacts));
      setReport(res.report);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoadingMore(false);
    }
  }, [artifacts.length, includeIntermediate, remote]);

  // Runs a new library search for `query`. An invalid query shows the engine's message inline.
  const runLibrarySearch = useCallback(async (query: string) => {
    const seq = ++searchSeq.current;
    setLibraryBusy(true);
    try {
      const res = await searchLibrary({ query, limit: LIBRARY_PAGE });
      if (seq !== searchSeq.current) return;
      setLibraryHits(res.hits);
      setLibraryTotal(res.total);
      setLibraryCursor(res.next_cursor);
      setLibraryNotIndexed(res.not_indexed ?? []);
      setLibraryRebuilt(res.index?.rebuilt === true);
      setLibraryError(null);
    } catch (e) {
      if (seq !== searchSeq.current) return;
      setLibraryHits([]);
      setLibraryTotal(0);
      setLibraryCursor(null);
      setLibraryError(errorText(e));
    } finally {
      if (seq === searchSeq.current) setLibraryBusy(false);
    }
  }, []);

  // Re-runs the search after a submit, a delete, an import or Refresh. Nothing runs in the normal listing.
  useEffect(() => {
    if (libraryQuery !== null) void runLibrarySearch(libraryQuery);
  }, [libraryQuery, libraryRun, reloads, runLibrarySearch]);

  const loadMoreHits = useCallback(async () => {
    if (libraryQuery === null || libraryCursor === null) return;
    const seq = searchSeq.current;
    const cursor = libraryCursor;
    setLibraryMoreBusy(true);
    try {
      const res = await searchLibrary({ query: libraryQuery, limit: LIBRARY_PAGE, cursor });
      if (seq !== searchSeq.current) return;
      setLibraryHits((prev) => [...prev, ...res.hits.filter((h) => !prev.some((p) => p.asset_id === h.asset_id))]);
      setLibraryCursor(res.next_cursor);
      setLibraryError(null);
    } catch (e) {
      if (seq === searchSeq.current) setLibraryError(errorText(e));
    } finally {
      if (seq === searchSeq.current) setLibraryMoreBusy(false);
    }
  }, [libraryQuery, libraryCursor]);

  function exitLibrary() {
    searchSeq.current += 1;
    setLibraryQuery(null);
    setLibraryHits([]);
    setLibraryTotal(0);
    setLibraryCursor(null);
    setLibraryError(null);
    setLibraryBusy(false);
    setLibraryMoreBusy(false);
  }

  function changeLibraryInput(value: string) {
    setLibraryInput(value);
    // An empty box returns to the normal gallery listing.
    if (value === '') exitLibrary();
  }

  function submitLibrary(event: FormEvent) {
    event.preventDefault();
    const query = libraryInput.trim();
    if (!query) {
      exitLibrary();
      return;
    }
    setLibraryQuery(query);
    setLibraryRun((n) => n + 1);
  }

  const generated = useMemo<Item[]>(
    () =>
      app.jobs.flatMap((j) =>
        j.assets.map((a): Item => {
          const name = baseName(a.path, `${j.label} ${a.asset_id.slice(-4)}`);
          const sub = `generated · ${relativeTime(j.createdAt)}`;
          return {
            id: `gen:${a.asset_id}`,
            name,
            sub,
            url: assetUrl(a.path),
            type: (a.media === 'video' || j.kind === 'video') ? 'video' : (a.media === 'audio' || j.kind === 'voice') ? 'audio' : 'image',
            path: a.path,
            project: 'generated',
            brand: 'NA',
            eyes: 'unknown',
            jury: 'unknown',
            metadata: 'complete',
            caller: '',
            callerLabel: '',
            haystack: `${name} ${sub} ${j.provider} ${j.model}`.toLowerCase(),
            prompt: j.prompt || null,
            artifact: null,
            facts: null,
            scope: null,
            rows: [
              ['Source', 'Generated in this app'], ['Provider', j.provider], ['Model', j.model], ['Cost', j.estUsd ? usd(j.estUsd) : 'Free'],
              ...(a.width && a.height ? [['Size', `${a.width} × ${a.height}`] as [string, string]] : []),
              ...(a.durationS != null ? [['Duration', formatDuration(a.durationS)] as [string, string]] : []),
              ...(a.mime ? [['Type', a.mime] as [string, string]] : [])
            ]
          };
        })
      ),
    [app.jobs]
  );

  const items = useMemo(
    () => [...generated, ...artifacts.map(fromArtifact)].filter((i) => !removedPaths.has(i.path)),
    [generated, artifacts, removedPaths]
  );
  const visible = useMemo(() => items.filter((i) => matchesFilters(i, filters)), [items, filters]);
  const libraryItems = useMemo(() => libraryHits.filter((h) => !removedPaths.has(h.path)).map(fromHit), [libraryHits, removedPaths]);
  const current = items.find((i) => i.id === selected) ?? libraryItems.find((i) => i.id === selected) ?? null;
  const active = activeFilterCount(filters);

  // Dropdown choices come from the unfiltered listing while the engine narrows the loaded one, so a
  // project, brand, eyes or jury filter keeps every other option. Library assets with no project are
  // the "No project" option (sent to the engine as no_project, since an empty project is refused).
  const optionItems = optionSourceItems(
    items,
    [...generated, ...unfiltered.map(fromArtifact)],
    isServerFiltered(remote)
  );
  const brandOptions = optionsFor(optionItems.map((i) => i.brand), BRAND_CODES, (c) => brandLabel(c, BRANDS));
  const eyesOptions = optionsFor(optionItems.map((i) => i.eyes), EYES_STATES, stateLabel);
  const juryOptions = optionsFor(optionItems.map((i) => i.jury), JURY_STATES, humanize);
  const metaOptions = optionsFor(optionItems.map((i) => i.metadata), METADATA_STATES, humanize);
  // "From the host app": only callers that registered something appear; files with no caller have an empty slug.
  const callerLabels = new Map(optionItems.filter((i) => i.caller).map((i) => [i.caller, i.callerLabel] as [string, string]));
  const callerOptions = optionsFor(optionItems.map((i) => i.caller).filter(Boolean), [], (slug) => callerLabel(slug, callerLabels));
  const projectOptions = optionsFor(optionItems.map((i) => i.project), [NO_PROJECT_FILTER], projectLabel);
  const brandInfo = current ? BRANDS.find((b) => b.code === current.brand) : undefined;
  const a = current?.artifact ?? null;
  const searching = libraryQuery !== null;
  // Remote and inline previews have no file on disk for the engine to remove; a missing library file has nothing to delete.
  const deletable = current !== null && !/^(data:|blob:|https?:)/i.test(current.path) && current.exists !== false;

  // The eyes gate releases an asset for delivery, so the approval is the app's: approveDraft mints the token.
  // The hash is read at the click; the engine binds the approval to that path and those bytes.
  async function approveEyes() {
    if (!current) return;
    const target = current;
    setEyesBusy(true);
    try {
      const state = await studio<{ hash: string | null }>('eyes.status', { path: target.path });
      if (!state.hash) throw new Error('The file is not on disk, so there is nothing to approve.');
      await approveDraft('eyes', state.hash.replace(/^sha256:/, ''), state.hash, { path: target.path, stage: 'gallery' });
      setError(null);
      setReloads((n) => n + 1);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setEyesBusy(false);
    }
  }

  async function deleteCurrent() {
    if (!current) return;
    const target = current;
    const ok = await confirmAction({
      title: 'Delete this file?',
      paragraphs: ['This removes the file from disk and from the gallery.'],
      subject: target.name,
      note: 'This cannot be undone.',
      confirmLabel: 'Delete file',
      danger: true
    });
    if (!ok) return;
    try {
      const result = await deleteAsset({ path: target.path });
      if (!result.removed) {
        setError('Nothing was removed. The engine did not find this file in the library.');
        return;
      }
      setRemovedPaths((prev) => new Set(prev).add(target.path));
      setSelected(null);
      setError(null);
      setReloads((n) => n + 1);
    } catch (e) {
      setError(errorMessage(e));
    }
  }

  return (
    <div className="content view">
      <section className="work">
        <div className="eyebrow">Library</div>
        <h1>Gallery</h1>
        <p className="lede">Everything generated here plus artifacts indexed from your content folders. Filter by brand, human-eyes review and jury verdict.</p>
        <form className="lib-bar" role="search" aria-label="Library search" onSubmit={submitLibrary}>
          <Input type="search" className="lib-input" aria-label="Search library" placeholder='Search library: words, "phrases", kind:, origin:, rights:' value={libraryInput} onChange={(e) => changeLibraryInput(e.target.value)} />
          <Button type="submit" disabled={!libraryInput.trim() || libraryBusy}>Search</Button>
          <Button type="button" aria-expanded={importOpen} onClick={() => setImportOpen((open) => !open)}>Import file</Button>
        </form>
        {importOpen ? (
          <ImportPanel
            onClose={() => setImportOpen(false)}
            onImported={() => {
              setReloads((n) => n + 1);
              setLibraryRun((n) => n + 1);
            }}
          />
        ) : null}
        <div className="toolbar" style={{ marginTop: 16 }}>
          <Input type="search" className="toolbar-search" aria-label="Filter gallery" placeholder="Filter files…" value={filters.query} onChange={(e) => set('query', e.target.value)} />
          <SegmentedControl label="Type" value={filters.type} onChange={(v) => set('type', v)} options={[{ value: 'all', label: 'All' }, { value: 'image', label: 'Images' }, { value: 'video', label: 'Video' }, { value: 'audio', label: 'Audio' }]} />
          {searching ? null : <span style={{ marginLeft: 'auto' }}><Pill>{visible.length} of {items.length}</Pill></span>}
        </div>
        <div className="gal-filters" role="group" aria-label="Gallery filters">
          <Filter label="Brand" value={filters.brand} options={brandOptions} onChange={(v) => set('brand', v)} />
          <Filter label="Eyes" value={filters.eyes} options={eyesOptions} onChange={(v) => set('eyes', v)} />
          <Filter label="Jury" value={filters.jury} options={juryOptions} onChange={(v) => set('jury', v)} />
          <Filter label="Project" value={filters.project} options={projectOptions} onChange={(v) => set('project', v)} />
          {callerOptions.length > 0 || filters.caller !== ALL ? (
            <Filter label="Source" value={filters.caller} options={callerOptions} onChange={(v) => set('caller', v)} />
          ) : null}
          <Filter label="Metadata" value={filters.metadata} options={metaOptions} onChange={(v) => set('metadata', v)} />
          <Checkbox className="gal-check" checked={includeIntermediate} onChange={setIncludeIntermediate} label="Intermediate" />
          <Button type="button" variant="ghost" size="sm" className="gal-clear" disabled={active === 0} onClick={() => setFilters(NO_FILTERS)}>Clear{active ? ` (${active})` : ''}</Button>
          <Button type="button" variant="ghost" size="sm" className="gal-clear" onClick={() => setReloads((n) => n + 1)}>Refresh</Button>
        </div>
        {searching ? <p className="lib-note">Gallery filters and the type filter apply to the gallery listing. Clear the search to use them.</p> : null}
        {error ? <Notice tone="bad">{error}</Notice> : null}
        {libraryQuery !== null ? (
          <LibraryResults
            query={libraryQuery}
            hits={libraryHits}
            total={libraryTotal}
            hasMore={libraryCursor !== null}
            notIndexed={libraryNotIndexed}
            rebuilt={libraryRebuilt}
            busy={libraryBusy}
            loadingMore={libraryMoreBusy}
            error={libraryError}
            selectedId={selected}
            onSelect={(hit) => setSelected(`lib:${hit.asset_id}`)}
            onLoadMore={() => void loadMoreHits()}
            onBack={() => {
              setLibraryInput('');
              exitLibrary();
            }}
          />
        ) : (
          <>
            {loading ? <EmptyState title="Indexing…" /> : null}
            {!loading && visible.length === 0 && !error ? (
              <EmptyState title={items.length ? 'No files match these filters' : 'Nothing here yet'}>
                {items.length ? 'Clear a filter to see more.' : 'Generate something, or point the engine at a content folder.'}
              </EmptyState>
            ) : null}
            <div className="grid">
              {/* RightKit ruling 12b1a381: N/A valid product UI (gallery tile) */}
              {visible.map((i) => (
                <div key={i.id} className="tile" data-brand={i.brand} data-eyes={i.eyes} data-jury={i.jury}>
                  <Card flush selected={selected === i.id} onClick={() => setSelected(i.id)} label={i.name}>
                  <span className={`img${i.type === 'audio' ? ' audio' : ''}`}>
                    {i.type === 'image' ? <img src={i.url} alt={i.name} loading="lazy" /> : i.type === 'video' ? <video src={i.url} muted preload="metadata" /> : <span>Audio</span>}
                    {i.artifact ? (
                      <span className="badges">
                        <Badge>{i.brand}</Badge>
                        {i.eyes !== 'unknown' ? <Badge tone={stateTone(i.eyes)}>eyes {stateLabel(i.eyes)}</Badge> : null}
                        {i.jury !== 'unknown' ? <Badge tone={stateTone(i.jury)}>jury {humanize(i.jury)}</Badge> : null}
                      </span>
                    ) : null}
                  </span>
                  <span className="meta"><b>{i.name}</b><span>{i.sub}</span></span>
                  </Card>
                </div>
              ))}
            </div>
            {report?.has_more ? (
              <div className="gal-more">
                <Button type="button" variant="ghost" size="sm" className="gal-clear" disabled={loadingMore} onClick={() => void loadMore()}>
                  {loadingMore ? 'Loading…' : `Load more (${artifacts.length} of ${report.total_available} indexed)`}
                </Button>
              </div>
            ) : null}
          </>
        )}
      </section>

      <aside className="run">
        <div className="run-head">
          <div className="eyebrow">Inspector</div>
          <h3 style={{ margin: '2px 0 0', fontSize: 15, fontWeight: 600, overflowWrap: 'anywhere' }}>{current?.name ?? 'No file selected'}</h3>
        </div>
        <div className="run-body">
          {current ? (
            <>
              <div className="preview">
                {current.exists === false ? (
                  <div className="lib-missing">The file is no longer on disk. The library record remains.</div>
                ) : current.type === 'image' ? (
                  <ImageViewer key={current.path} path={current.path} src={current.url} displayName={current.name} readOnly />
                ) : (
                  <MediaPlayer key={current.url} src={current.url} kind={current.type === 'video' ? 'video' : 'audio'} title={current.name} />
                )}
              </div>
              {a ? (
                <div className="chips" aria-label="Review state">
                  <StateChip label="Eyes" state={a.approval.eyes} />
                  <StateChip label="Jury" state={a.approval.auto_jury} />
                  <StateChip label="Overall" state={a.approval.overall} />
                </div>
              ) : null}
              {a && deletable && a.approval.eyes !== 'approved' ? (
                <div className="insp-sec">
                  <Button type="button" disabled={eyesBusy} onClick={() => void approveEyes()} title="Records that you looked at this file and approved it for delivery">
                    {eyesBusy ? 'Approving...' : a.approval.eyes === 'stale' ? 'Approve the edited file for export' : 'Approve for export'}
                  </Button>
                  <div className="foot-note" style={{ marginTop: 6 }}>Delivery to {hostAppName()} and {hostAppName()} waits for this approval. Editing the file afterwards withdraws it.</div>
                </div>
              ) : null}
              <Kv rows={current.rows} />
              {brandInfo ? <div className="brand-note" style={{ marginTop: 6 }}>{brandInfo.descriptor}{brandInfo.name_verified === false ? ' (name not confirmed)' : ''}</div> : null}
              <div className="insp-sec">
                <h6>Asset facts</h6>
                <Kv rows={factsRows(current.facts, current.scope)} />
                <div className="foot-note" style={{ marginTop: 6 }}>{NOT_RECORDED} means the engine holds no fact for this file. It is not a claim either way.</div>
              </div>
              {current.prompt ? <div className="insp-sec"><h6>Prompt</h6><div className="prompt selectable">{current.prompt}</div></div> : null}
              <div className="recent"><h6>Location</h6><div className="mono selectable">{current.path.startsWith('data:') ? '(inline preview)' : current.path}</div></div>
              <div className="insp-sec">
                <Button type="button" variant="danger" disabled={!deletable} onClick={() => void deleteCurrent()} title={deletable ? undefined : 'This preview has no file on disk to delete'}>Delete file</Button>
              </div>
              {a ? (
                <>
                  <div className="insp-sec">
                    <h6>Sidecars</h6>
                    <Kv rows={[
                      ['Eyes', a.sidecars.eyes_path ?? 'none'],
                      ['Jury verdict', a.sidecars.verdict_path ?? 'none'],
                      ['Metadata', a.sidecars.sidecar_path ?? 'none'],
                      ['Manifest', a.sidecars.manifest_path ?? 'none']
                    ].map(([k, v]) => [k, <span className="mono selectable" style={{ overflowWrap: 'anywhere' }}>{v === 'none' ? v : baseName(v, v)}</span>])} />
                  </div>
                  {a.diagnostics.length ? <div className="insp-sec"><h6>Diagnostics</h6><pre className="selectable">{JSON.stringify(a.diagnostics, null, 2)}</pre></div> : null}
                  {a.raw_sidecars && Object.keys(a.raw_sidecars).length ? (
                    <div className="insp-sec"><details><summary>Raw sidecars</summary><pre className="selectable">{JSON.stringify(a.raw_sidecars, null, 2)}</pre></details></div>
                  ) : null}
                </>
              ) : null}
            </>
          ) : <EmptyState title="Select a file">Preview it and see how it was made.</EmptyState>}
        </div>
        <div className="run-foot">
          <div className="foot-note" style={{ marginTop: 0 }}>
            {report ? `${artifacts.length}${report.total_available != null ? ` of ${report.total_available}` : ''} indexed · ${report.metadata_incomplete_count} incomplete · ${report.elapsed_ms} ms` : 'Index not loaded'}
          </div>
        </div>
      </aside>
    </div>
  );
}
