import { useState } from "react";
import { Badge, EmptyState, Field, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect.js";
import { LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import {
  groupDigits,
  instantLabel,
  type DataSource,
  type HookTestOpening,
  type HookTestSet,
  type OperatorScope,
} from "../data/index.js";
import { HOOK_ROLE_LABEL, HOOK_STATE_LABEL } from "./labels.js";

interface Props {
  source: DataSource;
  rev: number;
}

/** An instant from the engine, in UTC, or the words "Not reported" when the engine holds none. */
function instantOrNone(utc: string | null): string {
  return utc === null ? "Not reported" : instantLabel(utc, "UTC");
}

/** The role of one opening. Only a decided set has roles; an open set's openings are not decided. */
function roleText(opening: HookTestOpening): string {
  return opening.role === null ? "Not decided" : HOOK_ROLE_LABEL[opening.role];
}

export function HookTestsScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Hook tests"
      subtitle="Read only. Variant sets: one body with several openings, each published as a trial reel, and the winner once the set is decided. Nothing here creates a set, binds a job or reads a set."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Hook tests are kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's sets. */}
          <HookTestView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function HookTestView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const { state, reload, value } = useAsync<HookTestSet[]>(
    () => source.listHookTests(scopeId),
    [source, rev, scopeId],
  );
  const [picked, setPicked] = useState<string | null>(null);

  if (state.status === "error") {
    return <LoadError message="Could not read the hook tests from the daemon." onRetry={reload} />;
  }
  if (value === null) return <SkeletonRows label="Loading hook tests" rows={3} />;
  if (value.length === 0) {
    return (
      <EmptyState title="No hook tests yet">
        <p>This channel has no hook test. A set is made on the daemon host, not here. This is not an error.</p>
      </EmptyState>
    );
  }

  const selected = value.find((s) => s.id === picked) ?? value[0];

  return (
    <>
      <section aria-labelledby="hook-list-title">
        <h2 id="hook-list-title" className="op-section-title">
          Hook tests ({value.length})
        </h2>
        <div className="op-table-scroll">
          <Table aria-labelledby="hook-list-title">
            <TableCaption>Variant sets in this channel</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Set</TableHeaderCell>
                <TableHeaderCell>Brief</TableHeaderCell>
                <TableHeaderCell>State</TableHeaderCell>
                <TableHeaderCell>Metric and window</TableHeaderCell>
                <TableHeaderCell align="end">
                  Openings bound
                </TableHeaderCell>
                <TableHeaderCell>Created (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {value.map((set) => {
                const badge = HOOK_STATE_LABEL[set.state];
                const bound = set.openings.filter((o) => o.jobId !== null).length;
                return (
                  <TableRow key={set.id}>
                    <TableCell rowHeader className="op-mono">
                      {set.id}
                    </TableCell>
                    <TableCell>{set.briefRef}</TableCell>
                    <TableCell>
                      <Badge tone={badge.tone}>{badge.text}</Badge>
                    </TableCell>
                    <TableCell>
                      {set.metric}, {set.window}
                    </TableCell>
                    <TableCell numeric>
                      {bound} of {set.openings.length}
                    </TableCell>
                    <TableCell className="op-mono">{instantLabel(set.createdAtUtc, "UTC")}</TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      </section>

      <section aria-labelledby="hook-detail-heading">
        <h2 id="hook-detail-heading" className="op-section-title">
          Selected hook test
        </h2>
        <Field label="Hook test">
          {({ id, "aria-describedby": describedBy }) => (
            <select
              id={id}
              aria-describedby={describedBy}
              className="op-select"
              value={selected.id}
              onChange={(e) => setPicked(e.target.value)}
            >
              {value.map((set) => (
                <option key={set.id} value={set.id}>
                  {set.briefRef} ({set.id})
                </option>
              ))}
            </select>
          )}
        </Field>
        {/* Keyed by set so a switch never shows the previous set's openings. */}
        <HookTestDetail key={selected.id} source={source} rev={rev} scopeId={scopeId} setId={selected.id} />
      </section>
    </>
  );
}

function HookTestDetail({
  source,
  rev,
  scopeId,
  setId,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  setId: string;
}) {
  const { state, reload } = useAsync<HookTestSet | null>(
    () => source.getHookTest(scopeId, setId),
    [source, rev, scopeId, setId],
  );

  if (state.status === "error") {
    return <LoadError message="Could not read this hook test from the daemon." onRetry={reload} />;
  }
  if (state.status === "loading") return <SkeletonRows label="Loading the hook test" rows={3} />;
  const set = state.value;
  if (set === null) return <p className="op-hint">This hook test is no longer in this channel.</p>;

  const badge = HOOK_STATE_LABEL[set.state];

  return (
    <article className="op-detail" aria-labelledby="hook-detail-title">
      <h3 id="hook-detail-title" className="op-detail__title">
        {set.briefRef}
      </h3>
      <div className="op-badges">
        <Badge tone={badge.tone}>{badge.text}</Badge>
      </div>

      {set.state === "open" ? (
        <p className="op-hint">
          This set is open. Each opening is read 24 hours after its job is published. That reading happens on the
          server, not here. This screen shows the set as it stands and does not read it or decide it.
        </p>
      ) : (
        <p className="op-hint">
          Decided {instantOrNone(set.decidedAtUtc)}. Opening {set.winnerIndex ?? "not recorded"} was promoted and the
          other openings were archived. Each keeps the number it was compared on, when it was observed and the pull
          that stored it.
        </p>
      )}

      <dl className="op-dl">
        <dt>Metric and window</dt>
        <dd>
          {set.metric}, {set.window}
        </dd>
        <dt>Revision</dt>
        <dd>{set.revision}</dd>
        <dt>Created</dt>
        <dd>{instantLabel(set.createdAtUtc, "UTC")}</dd>
        <dt>Last changed</dt>
        <dd>{instantLabel(set.updatedAtUtc, "UTC")}</dd>
      </dl>

      <h4 className="op-section-title">Body</h4>
      <div className="op-caption">{set.body}</div>

      <h4 id="hook-openings-title" className="op-section-title">
        Openings
      </h4>
      <div className="op-table-scroll">
        <Table aria-labelledby="hook-openings-title">
          <TableCaption>
            {set.state === "open" ? "Openings and their bound jobs" : "Openings with their numbers and lineage"}
          </TableCaption>
          <TableHead>
            <TableRow>
              <TableHeaderCell>Opening</TableHeaderCell>
              <TableHeaderCell>Bound job</TableHeaderCell>
              <TableHeaderCell>Role</TableHeaderCell>
              <TableHeaderCell align="end">
                {set.metric} value
              </TableHeaderCell>
              <TableHeaderCell>Observed (UTC)</TableHeaderCell>
              <TableHeaderCell>Pull</TableHeaderCell>
              <TableHeaderCell>Published (UTC)</TableHeaderCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {set.openings.map((opening) => (
              <TableRow key={opening.index}>
                <TableCell rowHeader>
                  {opening.index}. {opening.text}
                </TableCell>
                <TableCell className="op-mono">{opening.jobId ?? "Not bound yet"}</TableCell>
                <TableCell>{roleText(opening)}</TableCell>
                <TableCell numeric>{opening.value === null ? "Not read" : groupDigits(opening.value)}</TableCell>
                <TableCell className="op-mono">{instantOrNone(opening.observedAtUtc)}</TableCell>
                <TableCell className="op-mono">{opening.pullId ?? "None"}</TableCell>
                <TableCell className="op-mono">{instantOrNone(opening.publishedAtUtc)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </article>
  );
}
