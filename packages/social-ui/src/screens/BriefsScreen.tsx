import { useState } from "react";
import { Badge, EmptyState, Field, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect";
import { LoadError, MockBanner } from "../components/Shared";
import { useAsync } from "../hooks/useAsync";
import {
  groupDigits,
  instantLabel,
  type DataSource,
  type DeliveryEnvelope,
  type DispatchAttempt,
  type OperatorScope,
  type OutgoingBrief,
  type OutgoingBriefDetail,
} from "../data";
import { BRIEF_STATE_LABEL, DISPATCH_OUTCOME_LABEL } from "./labels";

interface Props {
  source: DataSource;
  rev: number;
}

export function BriefsScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Briefs and deliveries"
      subtitle="Read only. The outgoing content briefs of one channel, the delivery envelope each engine returned, and every dispatch attempt. Nothing here issues a brief, accepts a delivery or runs an engine."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Outgoing briefs are kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's briefs. */}
          <BriefsView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function BriefsView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const { state, reload, value } = useAsync<OutgoingBrief[]>(
    () => source.listBriefs(scopeId),
    [source, rev, scopeId],
  );
  const [picked, setPicked] = useState<string | null>(null);

  if (state.status === "error") {
    return <LoadError message="Could not read the outgoing briefs from the daemon." onRetry={reload} />;
  }
  if (value === null) return <SkeletonRows label="Loading outgoing briefs" rows={3} />;
  if (value.length === 0) {
    return (
      <EmptyState title="No outgoing briefs yet">
        <p>This channel has no outgoing brief. A brief is issued on the daemon host or with the CLI, not here. This is not an error.</p>
      </EmptyState>
    );
  }

  const selected = value.find((b) => b.requestId === picked) ?? value[0];

  return (
    <>
      <section aria-labelledby="briefs-list-title">
        <h2 id="briefs-list-title" className="op-section-title">
          Outgoing briefs ({value.length})
        </h2>
        <div className="op-table-scroll">
          <Table aria-labelledby="briefs-list-title">
            <TableCaption>Outgoing briefs in this channel</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Request id</TableHeaderCell>
                <TableHeaderCell>Brief id</TableHeaderCell>
                <TableHeaderCell align="end">
                  Revision
                </TableHeaderCell>
                <TableHeaderCell>State</TableHeaderCell>
                <TableHeaderCell>Created (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {value.map((brief) => {
                const label = BRIEF_STATE_LABEL[brief.state];
                return (
                  <TableRow key={brief.requestId}>
                    <TableCell rowHeader className="op-mono">
                      {brief.requestId}
                    </TableCell>
                    <TableCell className="op-mono">{brief.briefId}</TableCell>
                    <TableCell numeric>{groupDigits(String(brief.revision))}</TableCell>
                    <TableCell>
                      <Badge tone={label.tone}>{label.text}</Badge>
                    </TableCell>
                    <TableCell className="op-mono">{instantLabel(brief.createdAtUtc, "UTC")}</TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      </section>

      <section aria-labelledby="brief-detail-heading">
        <h2 id="brief-detail-heading" className="op-section-title">
          Selected brief
        </h2>
        <Field label="Brief">
          {({ id, "aria-describedby": describedBy }) => (
            <select
              id={id}
              aria-describedby={describedBy}
              className="op-select"
              value={selected.requestId}
              onChange={(e) => setPicked(e.target.value)}
            >
              {value.map((brief) => (
                <option key={brief.requestId} value={brief.requestId}>
                  {brief.requestId} ({brief.briefId})
                </option>
              ))}
            </select>
          )}
        </Field>
        {/* Keyed by request id so a switch never shows the previous brief's document, delivery or attempts. */}
        <BriefDetail key={selected.requestId} source={source} rev={rev} scopeId={scopeId} requestId={selected.requestId} />
      </section>
    </>
  );
}

function BriefDetail({
  source,
  rev,
  scopeId,
  requestId,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  requestId: string;
}) {
  const brief = useAsync<OutgoingBriefDetail | null>(
    () => source.getBrief(scopeId, requestId),
    [source, rev, scopeId, requestId],
  );
  const runs = useAsync<DispatchAttempt[]>(
    () => source.listDispatchAttempts(scopeId, requestId),
    [source, rev, scopeId, requestId],
  );
  const envelopes = useAsync<DeliveryEnvelope[]>(() => source.listDeliveries(scopeId), [source, rev, scopeId]);

  if (brief.state.status === "error") {
    return <LoadError message="Could not read this brief from the daemon." onRetry={brief.reload} />;
  }
  if (runs.state.status === "error") {
    return <LoadError message="Could not read the dispatch attempts from the daemon." onRetry={runs.reload} />;
  }
  if (envelopes.state.status === "error") {
    return <LoadError message="Could not read the deliveries from the daemon." onRetry={envelopes.reload} />;
  }
  if (brief.state.status === "loading" || runs.state.status === "loading" || envelopes.state.status === "loading") {
    return <SkeletonRows label="Loading the brief" rows={3} />;
  }

  const record = brief.state.value;
  if (record === null) return <p className="op-hint">This brief is no longer in this channel.</p>;
  const attempts = runs.state.value;
  const requestEnvelopes = envelopes.state.value.filter((d) => d.requestId === requestId);
  const state = BRIEF_STATE_LABEL[record.state];
  const summary = record.summary;

  return (
    <article className="op-detail" aria-labelledby="brief-detail-title">
      <h3 id="brief-detail-title" className="op-detail__title">
        {record.requestId}
      </h3>
      <div className="op-badges">
        <Badge tone={state.tone}>{state.text}</Badge>
      </div>

      <dl className="op-dl">
        <dt>Brief id</dt>
        <dd className="op-mono">{record.briefId}</dd>
        <dt>Revision</dt>
        <dd>{groupDigits(String(record.revision))}</dd>
        <dt>Created (UTC)</dt>
        <dd>{instantLabel(record.createdAtUtc, "UTC")}</dd>
        <dt>Last changed (UTC)</dt>
        <dd>{instantLabel(record.updatedAtUtc, "UTC")}</dd>
      </dl>

      <h4 className="op-section-title">Document summary</h4>
      <dl className="op-dl">
        <dt>Brief type</dt>
        <dd>{orNotStated(summary.briefType)}</dd>
        <dt>Platforms</dt>
        <dd>{listOrNone(summary.platforms)}</dd>
        <dt>Deliverables</dt>
        <dd>{listOrNone(summary.deliverables)}</dd>
        <dt>Hook</dt>
        <dd>{orNotStated(summary.hook)}</dd>
        <dt>Review mode</dt>
        <dd>{orNotStated(summary.reviewMode)}</dd>
        <dt>Review policy revision</dt>
        <dd>{summary.policyRevision === null ? "Not stated" : groupDigits(String(summary.policyRevision))}</dd>
        <dt>Stages</dt>
        <dd>{groupDigits(String(summary.stageCount))}</dd>
      </dl>

      <h4 className="op-section-title">Full document</h4>
      <pre className="op-pre" tabIndex={0} aria-label="Full brief document as stored">
        {JSON.stringify(record.document, null, 2)}
      </pre>

      <h4 className="op-section-title">Delivery</h4>
      {requestEnvelopes.length > 0 ? (
        <>
          {record.state === "partial" ? (
            <p className="op-hint">
              Partial: the editor engine produced only some of what was requested. The envelopes below are the ones it
              delivered; what it skipped or refused is listed under the dispatch attempts.
            </p>
          ) : null}
          {requestEnvelopes.map((envelope) => (
            <EnvelopeDetail
              key={envelope.envelopeId}
              source={source}
              rev={rev}
              scopeId={scopeId}
              envelopeId={envelope.envelopeId}
            />
          ))}
        </>
      ) : record.state === "delivered" ? (
        <p className="op-hint">This brief is marked delivered, but no envelope for it is stored in this channel.</p>
      ) : (
        <p className="op-hint">No delivery has been accepted for this brief yet.</p>
      )}

      <h4 className="op-section-title">Dispatch attempts</h4>
      {attempts.length === 0 ? (
        <p className="op-hint">No dispatch attempt has been made for this brief.</p>
      ) : (
        <AttemptsTable attempts={attempts} />
      )}
    </article>
  );
}

/** The delivery envelope the engine accepted for a brief, read with delivery.get. */
function EnvelopeDetail({
  source,
  rev,
  scopeId,
  envelopeId,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  envelopeId: string;
}) {
  const { state, reload } = useAsync<DeliveryEnvelope | null>(
    () => source.getDelivery(scopeId, envelopeId),
    [source, rev, scopeId, envelopeId],
  );

  if (state.status === "error") {
    return <LoadError message="Could not read this delivery from the daemon." onRetry={reload} />;
  }
  if (state.status === "loading") return <SkeletonRows label="Loading the delivery" rows={2} />;
  const delivery = state.value;
  if (delivery === null) return <p className="op-hint">This delivery is no longer in this channel.</p>;

  return (
    <>
      <dl className="op-dl">
        <dt>Deliverable</dt>
        <dd className="op-mono">{delivery.deliverable === "" ? "Not named" : delivery.deliverable}</dd>
        <dt>Envelope id</dt>
        <dd className="op-mono">{delivery.envelopeId}</dd>
        <dt>Brief id</dt>
        <dd className="op-mono">{delivery.briefId}</dd>
        <dt>Brief revision</dt>
        <dd>{groupDigits(String(delivery.briefRevision))}</dd>
        <dt>Accepted (UTC)</dt>
        <dd>{instantLabel(delivery.acceptedAtUtc, "UTC")}</dd>
      </dl>
      <p className="op-hint">The envelope as the engine stored it:</p>
      <pre
        className="op-pre"
        tabIndex={0}
        aria-label={`Delivery envelope for ${delivery.deliverable === "" ? "an unnamed deliverable" : delivery.deliverable} as stored`}
      >
        {JSON.stringify(delivery.document, null, 2)}
      </pre>
    </>
  );
}

/** Every dispatch attempt of one brief. Stderr is shown in a block of its own, so it is never read as prose. */
function AttemptsTable({ attempts }: { attempts: DispatchAttempt[] }) {
  return (
    <div className="op-table-scroll">
      <Table>
        <TableCaption>Dispatch attempts of this brief, oldest first</TableCaption>
        <TableHead>
          <TableRow>
            <TableHeaderCell align="end">
              Attempt
            </TableHeaderCell>
            <TableHeaderCell>Engine</TableHeaderCell>
            <TableHeaderCell>Outcome</TableHeaderCell>
            <TableHeaderCell>Started (UTC)</TableHeaderCell>
            <TableHeaderCell>Finished (UTC)</TableHeaderCell>
            <TableHeaderCell align="end">
              Exit code
            </TableHeaderCell>
            <TableHeaderCell>Message</TableHeaderCell>
            <TableHeaderCell>Stderr tail</TableHeaderCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {attempts.map((run) => {
            const label = DISPATCH_OUTCOME_LABEL[run.outcome];
            return (
              <TableRow key={run.attemptId}>
                <TableCell rowHeader numeric>
                  {groupDigits(String(run.attemptId))}
                </TableCell>
                <TableCell>{run.engine}</TableCell>
                <TableCell>
                  <Badge tone={label.tone}>{label.text}</Badge>
                </TableCell>
                <TableCell className="op-mono">{instantLabel(run.startedAtUtc, "UTC")}</TableCell>
                <TableCell className="op-mono">
                  {run.finishedAtUtc === null ? "Not finished" : instantLabel(run.finishedAtUtc, "UTC")}
                </TableCell>
                <TableCell numeric>{run.exitCode === null ? "None" : String(run.exitCode)}</TableCell>
                <TableCell>
                  {run.message ?? "None"}
                  {run.producedDeliverables.length > 0 ? (
                    <p className="op-hint">Envelopes for: {run.producedDeliverables.join(", ")}</p>
                  ) : null}
                  {run.skipped.length > 0 ? (
                    <ul aria-label={`Skipped by attempt ${run.attemptId}`}>
                      {run.skipped.map((entry, index) => (
                        <li key={`${entry.deliverable ?? "unnamed"}-${index}`}>
                          Skipped {entry.deliverable ?? "a deliverable"}: {entry.code ?? "no code"}
                          {entry.message === null ? "" : ` (${entry.message})`}
                        </li>
                      ))}
                    </ul>
                  ) : null}
                </TableCell>
                <TableCell>
                  {run.stderrTail === null || run.stderrTail === "" ? (
                    "None kept"
                  ) : (
                    <pre className="op-pre" tabIndex={0} aria-label={`Stderr tail of attempt ${run.attemptId}`}>
                      {run.stderrTail}
                    </pre>
                  )}
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </div>
  );
}

function listOrNone(items: readonly string[]): string {
  return items.length === 0 ? "None" : items.join(", ");
}

function orNotStated(text: string | null): string {
  return text === null || text === "" ? "Not stated" : text;
}
