import { Fragment, useState } from "react";
import { Badge, EmptyState, Field, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect";
import { LoadError, MockBanner } from "../components/Shared";
import { useAsync } from "../hooks/useAsync";
import {
  groupDigits,
  instantLabel,
  type ChannelMemoryDetail,
  type ChannelMemoryRevision,
  type DataSource,
  type JsonValue,
  type OperatorScope,
  type ProofRecord,
  type ProofStatus,
} from "../data";
import { PROOF_STATUS_LABEL } from "./labels";

interface Props {
  source: DataSource;
  rev: number;
}

/** The filter's choices. "all" lists every status; each other value matches one stored status. */
type ProofFilter = "all" | ProofStatus;

const PROOF_FILTERS: readonly { value: ProofFilter; label: string }[] = [
  { value: "all", label: "All statuses" },
  { value: "collected", label: "Collected" },
  { value: "approved", label: "Approved" },
  { value: "withdrawn", label: "Withdrawn" },
  { value: "superseded", label: "Superseded" },
];

/** The document sections the screen shows, in order. Each key is the document's own top-level key. */
const MEMORY_SECTIONS: readonly { key: string; title: string }[] = [
  { key: "voice", title: "Voice" },
  { key: "pillars", title: "Pillars" },
  { key: "cadence", title: "Cadence" },
  { key: "bannedMoves", title: "Banned moves" },
];

export function ChannelMemoryScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Channel memory"
      subtitle="Read only. The channel memory document of one channel: its latest revision and revision history, and its proof bank with each proof's status. Nothing here writes a memory revision, imports a proof, changes a proof's status or approves a proof."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Channel memory is kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's memory or proofs. */}
          <MemoryView key={`memory:${scopeId}`} source={source} rev={rev} scopeId={scopeId} />
          <ProofView key={`proofs:${scopeId}`} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function MemoryView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const latest = useAsync<ChannelMemoryDetail | null>(() => source.channelMemory(scopeId), [source, rev, scopeId]);
  const history = useAsync<ChannelMemoryRevision[]>(
    () => source.channelMemoryHistory(scopeId),
    [source, rev, scopeId],
  );

  if (latest.state.status === "error") {
    return <LoadError message="Could not read the channel memory from the daemon." onRetry={latest.reload} />;
  }
  if (history.state.status === "error") {
    return <LoadError message="Could not read the revision history from the daemon." onRetry={history.reload} />;
  }
  if (latest.state.status === "loading" || history.state.status === "loading") {
    return <SkeletonRows label="Loading channel memory" rows={3} />;
  }

  // A null latest revision is the not_found state: nothing has been written for this channel yet.
  const detail = latest.state.value;
  const revisions = history.state.value;

  return (
    <>
      <section aria-labelledby="memory-latest-title">
        <h2 id="memory-latest-title" className="op-section-title">
          Channel memory
        </h2>
        {detail === null ? (
          <EmptyState title="No channel memory yet">
            <p>No channel memory has been written for this channel yet. This is not an error.</p>
          </EmptyState>
        ) : (
          <MemoryDocument detail={detail} />
        )}
      </section>

      <section aria-labelledby="memory-history-title">
        <h2 id="memory-history-title" className="op-section-title">
          Revision history
        </h2>
        {revisions.length === 0 ? (
          <p className="op-hint">No revision is stored for this channel.</p>
        ) : (
          <div className="op-table-scroll">
            <Table aria-labelledby="memory-history-title">
              <TableCaption>Revisions of this channel&apos;s memory, newest first</TableCaption>
              <TableHead>
                <TableRow>
                  <TableHeaderCell align="end">
                    Revision
                  </TableHeaderCell>
                  <TableHeaderCell>Author</TableHeaderCell>
                  <TableHeaderCell>Written (UTC)</TableHeaderCell>
                  <TableHeaderCell>Content hash</TableHeaderCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {revisions.map((row) => (
                  <TableRow key={row.revision}>
                    <TableCell rowHeader numeric>
                      {groupDigits(String(row.revision))}
                    </TableCell>
                    <TableCell>{row.author}</TableCell>
                    <TableCell className="op-mono">{instantLabel(row.createdAtUtc, "UTC")}</TableCell>
                    <TableCell className="op-mono">{row.contentHash}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </section>
    </>
  );
}

/** The latest revision: its metadata, then each section the document holds. A section it lacks is said to be absent. */
function MemoryDocument({ detail }: { detail: ChannelMemoryDetail }) {
  return (
    <article className="op-detail" aria-labelledby="memory-latest-doc">
      <h3 id="memory-latest-doc" className="op-detail__title">
        Revision {groupDigits(String(detail.revision))}
      </h3>
      <dl className="op-dl">
        <dt>Revision</dt>
        <dd>{groupDigits(String(detail.revision))}</dd>
        {detail.supersedes === null ? null : (
          <>
            <dt>Supersedes</dt>
            <dd>Revision {groupDigits(String(detail.supersedes))}</dd>
          </>
        )}
        <dt>Author</dt>
        <dd>{detail.author}</dd>
        <dt>Written (UTC)</dt>
        <dd>{instantLabel(detail.createdAtUtc, "UTC")}</dd>
        <dt>Content hash</dt>
        <dd className="op-mono">{detail.contentHash}</dd>
      </dl>

      {MEMORY_SECTIONS.map((section) => (
        <MemorySection key={section.key} title={section.title} value={sectionOf(detail.document, section.key)} />
      ))}
    </article>
  );
}

/** One top-level section of the document, or undefined when this revision does not hold it. */
function sectionOf(document: JsonValue, key: string): JsonValue | undefined {
  if (document === null || typeof document !== "object" || Array.isArray(document)) return undefined;
  return document[key];
}

function MemorySection({ title, value }: { title: string; value: JsonValue | undefined }) {
  const id = `memory-section-${title.toLowerCase().replace(/\s+/g, "-")}`;
  return (
    <section aria-labelledby={id}>
      <h4 id={id} className="op-section-title">
        {title}
      </h4>
      {value === undefined ? (
        <p className="op-hint">This revision has no {title.toLowerCase()} section.</p>
      ) : (
        <MemoryValue value={value} />
      )}
    </section>
  );
}

/** A stored value as the document holds it: text as written, lists as lists, objects as their own fields. */
function MemoryValue({ value }: { value: JsonValue }) {
  if (value === null) return <span>Not stated</span>;
  if (typeof value === "string") return <span>{value}</span>;
  if (typeof value === "number" || typeof value === "boolean") return <span>{String(value)}</span>;
  if (Array.isArray(value)) {
    return (
      <ul className="op-id-list">
        {value.map((item, index) => (
          <li key={index}>
            <MemoryValue value={item} />
          </li>
        ))}
      </ul>
    );
  }
  return (
    <dl className="op-dl">
      {Object.entries(value).map(([key, item]) => (
        <Fragment key={key}>
          <dt>{key}</dt>
          <dd>
            <MemoryValue value={item} />
          </dd>
        </Fragment>
      ))}
    </dl>
  );
}

function ProofView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const [filter, setFilter] = useState<ProofFilter>("all");
  const status = filter === "all" ? null : filter;
  const proofs = useAsync<ProofRecord[]>(
    () => source.listProofs(scopeId, status),
    [source, rev, scopeId, filter],
  );

  return (
    <section aria-labelledby="proof-bank-title">
      <h2 id="proof-bank-title" className="op-section-title">
        Proof bank
      </h2>
      <p className="op-hint">
        Only approved proofs can be cited by a script. Approving a proof happens outside this screen. This screen shows
        each proof&apos;s status and changes none of them.
      </p>

      <Field label="Proof status">
        {({ id, "aria-describedby": describedBy }) => (
          <select
            id={id}
            aria-describedby={describedBy}
            className="op-select"
            value={filter}
            onChange={(e) => {
              const next = PROOF_FILTERS.find((option) => option.value === e.target.value);
              if (next) setFilter(next.value);
            }}
          >
            {PROOF_FILTERS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        )}
      </Field>

      {proofs.state.status === "error" ? (
        <LoadError message="Could not read the proof bank from the daemon." onRetry={proofs.reload} />
      ) : proofs.state.status === "loading" ? (
        <SkeletonRows label="Loading the proof bank" rows={3} />
      ) : (
        <ProofTable proofs={proofs.state.value} filter={filter} />
      )}
    </section>
  );
}

function ProofTable({ proofs, filter }: { proofs: ProofRecord[]; filter: ProofFilter }) {
  if (proofs.length === 0) {
    return (
      <EmptyState title={filter === "all" ? "No proofs yet" : "No proofs with this status"}>
        <p>
          {filter === "all"
            ? "This channel has no proof in its proof bank yet. This is not an error."
            : "This channel has no proof with this status. This is not an error."}
        </p>
      </EmptyState>
    );
  }

  return (
    <div className="op-table-scroll">
      <Table aria-labelledby="proof-bank-title">
        <TableCaption>Proofs in this channel ({groupDigits(String(proofs.length))})</TableCaption>
        <TableHead>
          <TableRow>
            <TableHeaderCell>Proof</TableHeaderCell>
            <TableHeaderCell>Subject</TableHeaderCell>
            <TableHeaderCell align="end">
              Value
            </TableHeaderCell>
            <TableHeaderCell>Units</TableHeaderCell>
            <TableHeaderCell>Observed period</TableHeaderCell>
            <TableHeaderCell>Origin</TableHeaderCell>
            <TableHeaderCell>Status</TableHeaderCell>
            <TableHeaderCell>Superseding proof or withdrawal note</TableHeaderCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {proofs.map((proof) => {
            const badge = PROOF_STATUS_LABEL[proof.status];
            return (
              <TableRow key={proof.answerId}>
                <TableCell rowHeader className="op-mono">
                  {proof.proofId}
                </TableCell>
                <TableCell className="op-mono">{proof.subject}</TableCell>
                {/* The value is the engine's exact decimal text, shown as stored. It is never parsed or grouped. */}
                <TableCell numeric className="op-mono">{proof.value}</TableCell>
                <TableCell>{proof.units}</TableCell>
                <TableCell className="op-mono">{observedPeriod(proof)}</TableCell>
                <TableCell>{proof.origin}</TableCell>
                <TableCell>
                  <Badge tone={badge.tone}>{badge.text}</Badge>
                </TableCell>
                <TableCell>{noteText(proof)}</TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </div>
  );
}

function observedPeriod(proof: ProofRecord): string {
  return `${proof.observedFrom} to ${proof.observedTo === "" ? "no end date stated" : proof.observedTo}`;
}

/** The superseding proof id or the withdrawal note, in words. Other statuses carry neither. */
function noteText(proof: ProofRecord): string {
  if (proof.status === "superseded") {
    return proof.supersededBy === null
      ? "Superseded. The engine holds no replacing proof id."
      : `Superseded by ${proof.supersededBy}`;
  }
  if (proof.status === "withdrawn") {
    return proof.withdrawalNote ?? "Withdrawn. The engine holds no withdrawal note.";
  }
  return "Not applicable";
}
