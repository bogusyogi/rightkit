import { useState } from "react";
import { EmptyState, Field, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect.js";
import { LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import {
  instantLabel,
  type DataSource,
  type DigestApplication,
  type OperatorScope,
  type ResearchDigest,
} from "../data/index.js";
import { platformName } from "./labels.js";

interface Props {
  source: DataSource;
  rev: number;
}

export function DigestsScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Research digests"
      subtitle="Read only. The research digests of one channel: each finding with its source, and which findings became draft calendar entries. Nothing here fetches a source, applies a digest, writes a caption or approves a draft."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Research digests are kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's digests. */}
          <DigestView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function DigestView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const { state, reload, value } = useAsync<ResearchDigest[]>(
    () => source.listDigests(scopeId),
    [source, rev, scopeId],
  );
  const [picked, setPicked] = useState<string | null>(null);

  if (state.status === "error") {
    return <LoadError message="Could not read the research digests from the daemon." onRetry={reload} />;
  }
  if (value === null) return <SkeletonRows label="Loading research digests" rows={3} />;
  if (value.length === 0) {
    return (
      <EmptyState title="No research digests yet">
        <p>This channel has no digest. A digest is added on the daemon host, not here. This is not an error.</p>
      </EmptyState>
    );
  }

  const selected = value.find((d) => d.id === picked) ?? value[0];

  return (
    <>
      <section aria-labelledby="digest-list-title">
        <h2 id="digest-list-title" className="op-section-title">
          Research digests ({value.length})
        </h2>
        <div className="op-table-scroll">
          <Table aria-labelledby="digest-list-title">
            <TableCaption>Research digests in this channel</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Digest</TableHeaderCell>
                <TableHeaderCell>Digest date</TableHeaderCell>
                <TableHeaderCell align="end">
                  Findings
                </TableHeaderCell>
                <TableHeaderCell>Created (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {value.map((digest) => (
                <TableRow key={digest.id}>
                  <TableCell rowHeader className="op-mono">
                    {digest.id}
                  </TableCell>
                  <TableCell className="op-mono">{digest.digestDate}</TableCell>
                  <TableCell numeric>{digest.entries.length}</TableCell>
                  <TableCell className="op-mono">{instantLabel(digest.createdAtUtc, "UTC")}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </section>

      <section aria-labelledby="digest-detail-heading">
        <h2 id="digest-detail-heading" className="op-section-title">
          Selected digest
        </h2>
        <Field label="Digest">
          {({ id, "aria-describedby": describedBy }) => (
            <select
              id={id}
              aria-describedby={describedBy}
              className="op-select"
              value={selected.id}
              onChange={(e) => setPicked(e.target.value)}
            >
              {value.map((digest) => (
                <option key={digest.id} value={digest.id}>
                  {digest.digestDate} ({digest.id})
                </option>
              ))}
            </select>
          )}
        </Field>
        {/* Keyed by digest so a switch never shows the previous digest's findings. */}
        <DigestDetail key={selected.id} source={source} rev={rev} scopeId={scopeId} digestId={selected.id} />
      </section>
    </>
  );
}

function DigestDetail({
  source,
  rev,
  scopeId,
  digestId,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  digestId: string;
}) {
  const digest = useAsync<ResearchDigest | null>(
    () => source.getDigest(scopeId, digestId),
    [source, rev, scopeId, digestId],
  );
  const applied = useAsync<DigestApplication[] | null>(
    () => source.digestApplications(scopeId, digestId),
    [source, rev, scopeId, digestId],
  );

  if (digest.state.status === "error") {
    return <LoadError message="Could not read this digest from the daemon." onRetry={digest.reload} />;
  }
  if (applied.state.status === "error") {
    return <LoadError message="Could not read which findings were applied from the daemon." onRetry={applied.reload} />;
  }
  if (digest.state.status === "loading" || applied.state.status === "loading") {
    return <SkeletonRows label="Loading the digest" rows={3} />;
  }
  const record = digest.state.value;
  const applications = applied.state.value;
  if (record === null || applications === null) {
    return <p className="op-hint">This digest is no longer in this channel.</p>;
  }

  // The applied entry for each finding, keyed by the digest entry id.
  const appliedByEntry = new Map(applications.map((a) => [a.digestEntryId, a] as const));

  return (
    <article className="op-detail" aria-labelledby="digest-detail-title">
      <h3 id="digest-detail-title" className="op-detail__title">
        {record.id}
      </h3>

      <dl className="op-dl">
        <dt>Digest date</dt>
        <dd>{record.digestDate}</dd>
        <dt>Created</dt>
        <dd>{instantLabel(record.createdAtUtc, "UTC")}</dd>
        <dt>Findings</dt>
        <dd>{record.entries.length}</dd>
        <dt>Applied as draft calendar entries</dt>
        <dd>
          {applications.length} of {record.entries.length}
        </dd>
      </dl>

      <p className="op-hint">
        An applied finding became a draft calendar entry. Each draft carries a placeholder caption, so it cannot be
        approved until a person writes a real caption. This screen does not write that caption, approve the draft or
        apply the digest.
      </p>

      <h4 id="digest-entries-title" className="op-section-title">
        Findings
      </h4>
      <div className="op-table-scroll">
        <Table aria-labelledby="digest-entries-title">
          <TableCaption>Findings of {record.id}</TableCaption>
          <TableHead>
            <TableRow>
              <TableHeaderCell>Finding</TableHeaderCell>
              <TableHeaderCell>Topic</TableHeaderCell>
              <TableHeaderCell>Finding text</TableHeaderCell>
              <TableHeaderCell>Source</TableHeaderCell>
              <TableHeaderCell>Fetched</TableHeaderCell>
              <TableHeaderCell>Suggested platform</TableHeaderCell>
              <TableHeaderCell>Suggested format</TableHeaderCell>
              <TableHeaderCell>Draft calendar entry</TableHeaderCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {record.entries.map((entry) => {
              const application = appliedByEntry.get(entry.id);
              return (
                <TableRow key={entry.id}>
                  <TableCell rowHeader className="op-mono">
                    {entry.id}
                  </TableCell>
                  <TableCell>{entry.topic}</TableCell>
                  <TableCell>{entry.finding}</TableCell>
                  {/* Plain text, never a link: the source is shown as stored and is not opened here. */}
                  <TableCell className="op-mono">{entry.source}</TableCell>
                  <TableCell className="op-mono">{entry.fetchedOn}</TableCell>
                  <TableCell>{platformName(entry.platform)}</TableCell>
                  <TableCell>{entry.format}</TableCell>
                  <TableCell className="op-mono">{application ? application.calendarEntryId : "Not applied yet"}</TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>

      <h4 id="digest-applied-title" className="op-section-title">
        Applied findings
      </h4>
      {applications.length === 0 ? (
        <p className="op-hint">Nothing from this digest has been applied to the calendar yet.</p>
      ) : (
        <div className="op-table-scroll">
          <Table aria-labelledby="digest-applied-title">
            <TableCaption>Draft calendar entries made from this digest</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Finding</TableHeaderCell>
                <TableHeaderCell>Brief</TableHeaderCell>
                <TableHeaderCell>Draft calendar entry</TableHeaderCell>
                <TableHeaderCell>Applied (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {applications.map((application) => (
                <TableRow key={application.digestEntryId}>
                  <TableCell rowHeader className="op-mono">
                    {application.digestEntryId}
                  </TableCell>
                  <TableCell className="op-mono">{application.briefId}</TableCell>
                  <TableCell className="op-mono">{application.calendarEntryId}</TableCell>
                  <TableCell className="op-mono">{instantLabel(application.appliedAtUtc, "UTC")}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </article>
  );
}
