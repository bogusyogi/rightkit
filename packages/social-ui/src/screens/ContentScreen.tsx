import { useState } from "react";
import { Badge, EmptyState, Field, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect.js";
import { LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import {
  groupDigits,
  instantLabel,
  type ContentDetail,
  type ContentExperimentSummary,
  type ContentRecord,
  type ContentVariantCount,
  type DataSource,
  type OperatorScope,
} from "../data/index.js";
import {
  CONTENT_MEDIA_KIND_LABEL,
  CONTENT_STATE_LABEL,
  CONTENT_VARIANT_KIND_LABEL,
  CONTENT_VERDICT_LABEL,
} from "./labels.js";

interface Props {
  source: DataSource;
  rev: number;
}

/** An instant from the engine in UTC, or the words "Never expires" when the record has no expiry. */
function expiryText(utc: string | null): string {
  return utc === null ? "Never expires" : instantLabel(utc, "UTC");
}

export function ContentScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Content library"
      subtitle="Read only. The reusable media records of one channel: their state, expiry and cooldown, their caption and call-to-action variants, and the engine's comparison of those variants. Nothing here creates a record, plans an occurrence, withdraws a record or names a winner."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Content records are kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's records. */}
          <ContentView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function ContentView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const { state, reload, value } = useAsync<ContentRecord[]>(
    () => source.listContent(scopeId),
    [source, rev, scopeId],
  );
  const [picked, setPicked] = useState<string | null>(null);

  if (state.status === "error") {
    return <LoadError message="Could not read the content records from the daemon." onRetry={reload} />;
  }
  if (value === null) return <SkeletonRows label="Loading content records" rows={3} />;
  if (value.length === 0) {
    return (
      <EmptyState title="No content records yet">
        <p>This channel has no content record. A record is made on the daemon host, not here. This is not an error.</p>
      </EmptyState>
    );
  }

  const selected = value.find((r) => r.id === picked) ?? value[0];

  return (
    <>
      <section aria-labelledby="content-list-title">
        <h2 id="content-list-title" className="op-section-title">
          Content records ({value.length})
        </h2>
        <div className="op-table-scroll">
          <Table aria-labelledby="content-list-title">
            <TableCaption>Content records in this channel</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Title</TableHeaderCell>
                <TableHeaderCell>Content id</TableHeaderCell>
                <TableHeaderCell>State</TableHeaderCell>
                <TableHeaderCell align="end">
                  Revision
                </TableHeaderCell>
                <TableHeaderCell align="end">
                  Cooldown (days)
                </TableHeaderCell>
                <TableHeaderCell>Expires (UTC)</TableHeaderCell>
                <TableHeaderCell>Last changed (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {value.map((record) => {
                const badge = CONTENT_STATE_LABEL[record.state];
                return (
                  <TableRow key={record.id}>
                    <TableCell rowHeader>{record.title}</TableCell>
                    <TableCell className="op-mono">{record.id}</TableCell>
                    <TableCell>
                      <Badge tone={badge.tone}>{badge.text}</Badge>
                    </TableCell>
                    <TableCell numeric>{groupDigits(String(record.revision))}</TableCell>
                    <TableCell numeric>{groupDigits(String(record.cooldownDays))}</TableCell>
                    <TableCell className="op-mono">{expiryText(record.expiresAtUtc)}</TableCell>
                    <TableCell className="op-mono">{instantLabel(record.updatedAtUtc, "UTC")}</TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      </section>

      <section aria-labelledby="content-detail-heading">
        <h2 id="content-detail-heading" className="op-section-title">
          Selected content record
        </h2>
        <Field label="Content record">
          {({ id, "aria-describedby": describedBy }) => (
            <select
              id={id}
              aria-describedby={describedBy}
              className="op-select"
              value={selected.id}
              onChange={(e) => setPicked(e.target.value)}
            >
              {value.map((record) => (
                <option key={record.id} value={record.id}>
                  {record.title} ({record.id})
                </option>
              ))}
            </select>
          )}
        </Field>
        {/* Keyed by record so a switch never shows the previous record's variants or comparison. */}
        <ContentRecordDetail key={selected.id} source={source} rev={rev} scopeId={scopeId} contentId={selected.id} />
      </section>
    </>
  );
}

function ContentRecordDetail({
  source,
  rev,
  scopeId,
  contentId,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  contentId: string;
}) {
  const detail = useAsync<ContentDetail | null>(
    () => source.getContent(scopeId, contentId),
    [source, rev, scopeId, contentId],
  );
  const summary = useAsync<ContentExperimentSummary | null>(
    () => source.contentExperimentSummary(scopeId, contentId),
    [source, rev, scopeId, contentId],
  );

  if (detail.state.status === "error") {
    return <LoadError message="Could not read this record from the daemon." onRetry={detail.reload} />;
  }
  if (summary.state.status === "error") {
    return <LoadError message="Could not read the variant comparison from the daemon." onRetry={summary.reload} />;
  }
  if (detail.state.status === "loading" || summary.state.status === "loading") {
    return <SkeletonRows label="Loading the content record" rows={3} />;
  }
  const loaded = detail.state.value;
  if (loaded === null) return <p className="op-hint">This content record is no longer in this channel.</p>;
  const { record, variants } = loaded;
  const badge = CONTENT_STATE_LABEL[record.state];
  const comparison = summary.state.value;

  return (
    <article className="op-detail" aria-labelledby="content-detail-title">
      <h3 id="content-detail-title" className="op-detail__title">
        {record.title}
      </h3>
      <div className="op-badges">
        <Badge tone={badge.tone}>{badge.text}</Badge>
      </div>

      {record.state === "withdrawn" ? (
        <p className="op-hint">
          Withdrawn: {record.withdrawalReason ?? "no reason recorded"}. A withdrawn record is final. It cannot be edited,
          given variants or planned, and its planned occurrences were cancelled with it.
        </p>
      ) : null}
      {record.state === "expired" ? (
        <p className="op-hint">
          The clock is past this record&apos;s expiry, so it cannot be planned. The record itself is not changed.
        </p>
      ) : null}

      <dl className="op-dl">
        <dt>Content id</dt>
        <dd className="op-mono">{record.id}</dd>
        <dt>Evergreen</dt>
        <dd>{record.evergreen ? "Yes" : "No"}. Recorded for the operator; nothing acts on it yet.</dd>
        <dt>Cooldown</dt>
        <dd>{groupDigits(String(record.cooldownDays))} days between two occurrences on one platform</dd>
        <dt>Expires</dt>
        <dd>{expiryText(record.expiresAtUtc)}</dd>
        <dt>Revision</dt>
        <dd>{groupDigits(String(record.revision))}</dd>
        <dt>Created</dt>
        <dd>{instantLabel(record.createdAtUtc, "UTC")}</dd>
        <dt>Last changed</dt>
        <dd>{instantLabel(record.updatedAtUtc, "UTC")}</dd>
      </dl>

      <h4 className="op-section-title">Base caption</h4>
      <div className="op-caption">{record.baseCaption}</div>

      <h4 id="content-media-title" className="op-section-title">
        Media
      </h4>
      <div className="op-table-scroll">
        <Table aria-labelledby="content-media-title">
          <TableCaption>Media this record reuses</TableCaption>
          <TableHead>
            <TableRow>
              <TableHeaderCell>Hash</TableHeaderCell>
              <TableHeaderCell>Kind</TableHeaderCell>
              <TableHeaderCell align="end">
                Duration (ms)
              </TableHeaderCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {record.media.map((item, index) => (
              <TableRow key={`${item.hash}#${index}`}>
                <TableCell rowHeader className="op-mono">
                  {item.hash}
                </TableCell>
                <TableCell>{CONTENT_MEDIA_KIND_LABEL[item.kind]}</TableCell>
                <TableCell numeric>{item.durationMs === null ? "Not reported" : groupDigits(String(item.durationMs))}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>

      <h4 id="content-variants-title" className="op-section-title">
        Variants
      </h4>
      {variants.length === 0 ? (
        <p className="op-hint">No caption or call-to-action variant is stored for this record.</p>
      ) : (
        <div className="op-table-scroll">
          <Table aria-labelledby="content-variants-title">
            <TableCaption>Variants of {record.title}</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Variant id</TableHeaderCell>
                <TableHeaderCell>Kind</TableHeaderCell>
                <TableHeaderCell>Text</TableHeaderCell>
                <TableHeaderCell>Created (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {variants.map((variant) => (
                <TableRow key={variant.id}>
                  <TableCell rowHeader className="op-mono">
                    {variant.id}
                  </TableCell>
                  <TableCell>{CONTENT_VARIANT_KIND_LABEL[variant.kind]}</TableCell>
                  <TableCell>{variant.text}</TableCell>
                  <TableCell className="op-mono">{instantLabel(variant.createdAtUtc, "UTC")}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}

      <h4 id="content-compare-title" className="op-section-title">
        Variant comparison
      </h4>
      {comparison === null ? (
        <p className="op-hint">The engine holds no variant comparison for this record in this channel.</p>
      ) : (
        <ExperimentView summary={comparison} />
      )}
    </article>
  );
}

/** The engine's comparison. It shows the verdict and the engine's reason; it never names a winner. */
function ExperimentView({ summary }: { summary: ContentExperimentSummary }) {
  const verdict = CONTENT_VERDICT_LABEL[summary.verdict];
  return (
    <>
      <div className="op-badges">
        <Badge tone={verdict.tone}>{verdict.text}</Badge>
      </div>
      <p className="op-hint">Reason: {summary.reason}</p>
      <p className="op-hint">
        Each variant counts toward the verdict only with at least {groupDigits(String(summary.minOccurrences))}{" "}
        published occurrences, and a verdict needs at least two variants of each kind. The counts below are the
        occurrences the engine holds for each variant. The engine lists no occurrence on its own, so this screen cannot
        show them one by one. Counts are not engagement.
      </p>
      <VariantCountTable id="content-caption-counts-title" title="Caption variants" rows={summary.caption} />
      <VariantCountTable id="content-cta-counts-title" title="Call-to-action variants" rows={summary.cta} />
    </>
  );
}

function VariantCountTable({
  id,
  title,
  rows,
}: {
  id: string;
  title: string;
  rows: readonly ContentVariantCount[];
}) {
  return (
    <>
      <h4 id={id} className="op-section-title">
        {title}
      </h4>
      {rows.length === 0 ? (
        <p className="op-hint">None is stored, so there is nothing to compare.</p>
      ) : (
        <div className="op-table-scroll">
          <Table aria-labelledby={id}>
            <TableCaption>{title} and their occurrence counts</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Variant id</TableHeaderCell>
                <TableHeaderCell>Text</TableHeaderCell>
                <TableHeaderCell align="end">
                  Planned
                </TableHeaderCell>
                <TableHeaderCell align="end">
                  Published
                </TableHeaderCell>
                <TableHeaderCell align="end">
                  Cancelled
                </TableHeaderCell>
                <TableHeaderCell>Meets minimum</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {rows.map((row) => (
                <TableRow key={row.variantId}>
                  <TableCell rowHeader className="op-mono">
                    {row.variantId}
                  </TableCell>
                  <TableCell>{row.text}</TableCell>
                  <TableCell numeric>{groupDigits(String(row.planned))}</TableCell>
                  <TableCell numeric>{groupDigits(String(row.published))}</TableCell>
                  <TableCell numeric>{groupDigits(String(row.cancelled))}</TableCell>
                  <TableCell>{row.meetsMinimum ? "Yes" : "No"}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </>
  );
}
