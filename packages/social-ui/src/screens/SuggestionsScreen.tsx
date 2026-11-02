import { useState } from "react";
import { Badge, Button, EmptyState, Notice, PageShell, SkeletonRows } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect.js";
import { LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import {
  groupDigits,
  instantLabel,
  type DataSource,
  type OperatorScope,
  type OptimiseProposal,
  type OptimiseState,
} from "../data/index.js";
import { OPTIMISE_STATE_LABEL, platformName } from "./labels.js";

interface Props {
  source: DataSource;
  rev: number;
  onChanged: () => void;
}

type Notice = { tone: "ok" | "bad"; text: string } | null;

const SECTIONS: readonly { state: OptimiseState; title: string; empty: string }[] = [
  { state: "proposed", title: "Waiting for your decision", empty: "No suggestion is waiting for a decision." },
  { state: "accepted", title: "Accepted", empty: "No suggestion has been accepted." },
  { state: "dismissed", title: "Dismissed", empty: "No suggestion has been dismissed." },
];

/** Identifiers in the engine contain characters DOM ids do not allow here; keep letters, digits, "_" and "-". */
function domId(raw: string): string {
  return raw.replace(/[^A-Za-z0-9_-]/g, "_");
}

export function SuggestionsScreen({ source, rev, onChanged }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Suggestions"
      subtitle="Calendar changes proposed from stored analytics. Accepting one creates a draft calendar entry. Nothing is approved or published."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      <p className="op-hint">
        A draft entry describes the suggestion. Write its real caption on the calendar, then approve it there.
      </p>

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Suggestions appear for a channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect
            scopes={scopeList}
            value={scopeId}
            onChange={(id) => {
              setPicked(id);
              setNotice(null);
            }}
          />
          {notice ? (
            <Notice tone={notice.tone} className="op-notice">{notice.text}</Notice>
          ) : null}
          {/* Keyed by scope so a switch never shows the previous channel's suggestions. */}
          <ProposalList
            key={scopeId}
            source={source}
            rev={rev}
            scopeId={scopeId}
            onChanged={onChanged}
            onNotice={setNotice}
          />
        </>
      )}
    </PageShell>
  );
}

function ProposalList({
  source,
  rev,
  scopeId,
  onChanged,
  onNotice,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  onChanged: () => void;
  onNotice: (notice: Notice) => void;
}) {
  const { state, reload, value } = useAsync<OptimiseProposal[]>(
    () => source.listProposals(scopeId),
    [source, rev, scopeId],
  );

  if (state.status === "error") {
    return <LoadError message="Could not read suggestions from the daemon." onRetry={reload} />;
  }
  if (value === null) return <SkeletonRows label="Loading suggestions" rows={3} />;
  if (value.length === 0) {
    return (
      <EmptyState title="Not enough evidence yet">
        <p>
          This channel has no suggestion yet. A suggestion needs more published posts with stored analytics before the
          rule can compare them. This is not an error.
        </p>
      </EmptyState>
    );
  }

  return (
    <>
      {SECTIONS.map((section) => {
        const items = value.filter((proposal) => proposal.state === section.state);
        return (
          <section key={section.state} className="op-drafts-section" aria-labelledby={`suggestions-${section.state}`}>
            <h2 id={`suggestions-${section.state}`} className="op-section-title">
              {section.title} ({items.length})
            </h2>
            {items.length === 0 ? (
              <p className="op-hint">{section.empty}</p>
            ) : (
              <div className="op-drafts">
                {items.map((proposal) => (
                  <ProposalCard
                    key={proposal.id}
                    proposal={proposal}
                    scopeId={scopeId}
                    source={source}
                    onChanged={onChanged}
                    onNotice={onNotice}
                  />
                ))}
              </div>
            )}
          </section>
        );
      })}
    </>
  );
}

function ProposalCard({
  proposal,
  scopeId,
  source,
  onChanged,
  onNotice,
}: {
  proposal: OptimiseProposal;
  scopeId: string;
  source: DataSource;
  onChanged: () => void;
  onNotice: (notice: Notice) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [whyOpen, setWhyOpen] = useState(false);

  const id = domId(proposal.id);
  const badge = OPTIMISE_STATE_LABEL[proposal.state];

  async function decide(action: "accept" | "dismiss") {
    setBusy(true);
    const result =
      action === "accept"
        ? await source.acceptProposal(scopeId, proposal.id, proposal.revision)
        : await source.dismissProposal(scopeId, proposal.id, proposal.revision);
    setBusy(false);

    if (result.ok) {
      onNotice(
        action === "accept"
          ? {
              tone: "ok",
              text: `Draft calendar entry created: ${result.value.calendarEntryId ?? "(no id returned)"}. It still needs a real caption and approval before it can be scheduled or published.`,
            }
          : { tone: "ok", text: "Dismissed. No calendar entry was created." },
      );
      onChanged();
      return;
    }
    if (result.error.code === "revision_conflict") {
      onNotice({
        tone: "bad",
        text: "This suggestion changed after the list was read. Nothing was changed, and the list has been reloaded. Check it and decide again.",
      });
      onChanged();
      return;
    }
    onNotice({ tone: "bad", text: result.error.message });
  }

  return (
    <article className="op-card op-draft" aria-labelledby={`proposal-${id}`}>
      <h3 id={`proposal-${id}`} className="op-draft__title">
        {platformName(proposal.platform)}: remake a top-decile post
      </h3>
      <div className="op-badges">
        <Badge tone={badge.tone}>{badge.text}</Badge>
      </div>

      <dl className="op-dl">
        <dt>Rule</dt>
        <dd>Remake a post that is in the top decile for its channel.</dd>
        <dt>Source post</dt>
        <dd className="op-mono">{proposal.sourceJobId}</dd>
        <dt>Metric and window</dt>
        <dd>
          {proposal.metric}, {proposal.window}
        </dd>
        <dt>Value against threshold</dt>
        <dd>
          {groupDigits(proposal.value)} against a threshold of {groupDigits(proposal.threshold)}. A post wins when its
          value is at least the threshold.
        </dd>
        <dt>Sample</dt>
        <dd>
          {proposal.sampleSize} published posts. The top decile holds {proposal.decileSize}.
        </dd>
        <dt>Observed</dt>
        <dd>{instantLabel(proposal.observedAtUtc, "UTC")}</dd>
        <dt>Due</dt>
        <dd>{instantLabel(proposal.dueAtUtc, "UTC")}</dd>
        {proposal.decidedAtUtc ? (
          <>
            <dt>Decided</dt>
            <dd>
              {instantLabel(proposal.decidedAtUtc, "UTC")}
              {proposal.decisionNote ? `. Note: ${proposal.decisionNote}` : ""}
            </dd>
          </>
        ) : null}
        {proposal.calendarEntryId ? (
          <>
            <dt>Draft calendar entry</dt>
            <dd className="op-mono">{proposal.calendarEntryId}</dd>
          </>
        ) : null}
      </dl>

      <details onToggle={(e) => setWhyOpen(e.currentTarget.open)}>
        <summary>Why this was suggested</summary>
        {whyOpen ? <WhyDetail source={source} scopeId={scopeId} proposalId={proposal.id} /> : null}
      </details>

      {proposal.state === "proposed" ? (
        <div className="op-decide__buttons">
          <Button variant="primary" disabled={busy} onClick={() => decide("accept")}>
            Accept
          </Button>
          <Button variant="secondary" disabled={busy} onClick={() => decide("dismiss")}>
            Dismiss
          </Button>
        </div>
      ) : null}
    </article>
  );
}

/** The lineage of one suggestion, read when its "why" opens. Loads once per open. */
function WhyDetail({ source, scopeId, proposalId }: { source: DataSource; scopeId: string; proposalId: string }) {
  const { state, reload } = useAsync<OptimiseProposal | null>(
    () => source.getProposal(scopeId, proposalId),
    [source, scopeId, proposalId],
  );

  if (state.status === "error") {
    return <LoadError message="Could not read the lineage for this suggestion." onRetry={reload} />;
  }
  if (state.status === "loading") return <p className="op-hint">Loading the lineage.</p>;
  const detail = state.value;
  if (detail === null) return <p className="op-hint">This suggestion is no longer in this channel.</p>;

  return (
    <dl className="op-dl">
      <dt>Published posts compared</dt>
      <dd>
        <IdList ids={detail.lineageJobIds} />
      </dd>
      <dt>Analytics pulls compared</dt>
      <dd>
        <IdList ids={detail.lineagePullIds} />
      </dd>
    </dl>
  );
}

function IdList({ ids }: { ids: readonly string[] }) {
  if (ids.length === 0) return <>None recorded.</>;
  return (
    <ul className="op-id-list">
      {ids.map((id) => (
        <li key={id} className="op-mono">
          {id}
        </li>
      ))}
    </ul>
  );
}
