import { useState } from "react";
import { Badge, Button, EmptyState, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect";
import { LoadError, MockBanner } from "../components/Shared";
import { useAsync } from "../hooks/useAsync";
import {
  groupDigits,
  instantLabel,
  type DataSource,
  type InboxEvent,
  type InboxListing,
  type InboxRaw,
  type InboxStats,
  type InboxStatsGroup,
  type OperatorScope,
  type UnreadableRow,
} from "../data";
import { inboxKindText, inboxStateText, platformName } from "./labels";
import { hostAppName } from "../appName";

interface Props {
  source: DataSource;
  rev: number;
}

/**
 * The inbox of the signed webhook events. Every text, id and time in an event was written by the sender. The
 * screen renders each one as a React text child: never as markup, a link, an attribute or a class. Keys are the
 * engine's event ids. Nothing here sends a reply, marks an event or changes its state.
 */
export function InboxScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const stats = useAsync<InboxStats>(() => source.inboxStats(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Inbox"
      subtitle="Read only. The signed webhook events the daemon stored for one channel account, newest first, and the events no account matched. Nothing here replies, marks, ignores or handles an event."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      <p className="op-hint">
        Event text is shown exactly as received, as plain text. {hostAppName()} sends no reply from this screen.
      </p>

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>The inbox is kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {stats.state.status === "error" ? (
            <LoadError message="Could not read the inbox counts from the daemon." onRetry={stats.reload} />
          ) : stats.value === null ? (
            <SkeletonRows label="Loading the inbox counts" rows={1} />
          ) : (
            <InboxCounts stats={stats.value} scopeId={scopeId} />
          )}
          {/* Keyed by scope so a switch never shows the previous channel's events or selected event. */}
          <EventSection
            key={`scope:${scopeId}`}
            source={source}
            rev={rev}
            scopeId={scopeId}
            headingId="inbox-scope-title"
            title="Events of this channel"
            caption="Webhook events bound to this channel account, newest first"
            emptyText="Nothing has arrived for this channel yet."
          />
        </>
      )}

      <EventSection
        key="unmatched"
        source={source}
        rev={rev}
        scopeId={null}
        headingId="inbox-unmatched-title"
        title="Not matched to an account"
        caption="Webhook events no stored account matched, newest first"
        emptyText="No event is waiting here: every stored event matched an account."
      />
    </PageShell>
  );
}

/** The counts of the whole store, and this channel's counts by kind and state as stored. Counts are never rates. */
function InboxCounts({ stats, scopeId }: { stats: InboxStats; scopeId: string }) {
  const channelGroups = stats.groups.filter(
    (row): row is InboxStatsGroup => !("readable" in row) && row.scopeId === scopeId,
  );
  const unreadableGroups = stats.groups.filter((row) => "readable" in row).length;

  return (
    <section aria-labelledby="inbox-counts-title">
      <h2 id="inbox-counts-title" className="op-section-title">
        Counts
      </h2>
      <p className="op-count">
        Whole store, all channels: {groupDigits(String(stats.totalEvents))} events stored,{" "}
        {groupDigits(String(stats.unmatchedEvents))} not matched to an account.
      </p>
      <p className="op-count">
        Deliveries stored: {groupDigits(String(stats.deliveriesParsed))} read,{" "}
        {groupDigits(String(stats.deliveriesUnparseable))} could not be read.
      </p>
      <p className="op-count">
        Deliveries refused for a bad signature (whole store): {groupDigits(String(stats.refusedTotal))}.
      </p>
      <p className="op-hint">
        The refusal count is not kept per channel. A refused delivery is never stored, so it adds no event.
      </p>
      {channelGroups.length === 0 ? (
        <p className="op-hint">No event is counted for this channel.</p>
      ) : (
        <div className="op-table-scroll">
          <Table aria-labelledby="inbox-counts-title">
            <TableCaption>Events of this channel by kind and state, as stored</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Kind</TableHeaderCell>
                <TableHeaderCell>State</TableHeaderCell>
                <TableHeaderCell align="end">
                  Count
                </TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {channelGroups.map((group, index) => (
                <TableRow key={`count:${index}`}>
                  <TableCell rowHeader>{inboxKindText(group.kind)}</TableCell>
                  <TableCell>{inboxStateText(group.state).text}</TableCell>
                  <TableCell numeric className="op-mono">{groupDigits(String(group.count))}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      {unreadableGroups === 0 ? null : (
        <p className="op-hint">
          {groupDigits(String(unreadableGroups))} count row{unreadableGroups === 1 ? " could" : "s could"} not be read,
          and {unreadableGroups === 1 ? "is" : "are"} not in the table above.
        </p>
      )}
    </section>
  );
}

/** One list of events: a channel account's, or the unmatched ones. Each keeps its own selection. */
function EventSection({
  source,
  rev,
  scopeId,
  headingId,
  title,
  caption,
  emptyText,
}: {
  source: DataSource;
  rev: number;
  scopeId: string | null;
  headingId: string;
  title: string;
  caption: string;
  emptyText: string;
}) {
  const listing = useAsync<InboxListing>(() => source.listInbox(scopeId), [source, rev, scopeId]);
  const [picked, setPicked] = useState<string | null>(null);

  if (listing.state.status === "error") {
    return (
      <section aria-labelledby={headingId}>
        <h2 id={headingId} className="op-section-title">
          {title}
        </h2>
        <LoadError message="Could not read these events from the daemon." onRetry={listing.reload} />
      </section>
    );
  }
  if (listing.state.status === "loading") {
    return (
      <section aria-labelledby={headingId}>
        <h2 id={headingId} className="op-section-title">
          {title}
        </h2>
        <SkeletonRows label="Loading the events" rows={3} />
      </section>
    );
  }

  const { rows, moreAvailable } = listing.state.value;
  if (rows.length === 0) {
    return (
      <section aria-labelledby={headingId}>
        <h2 id={headingId} className="op-section-title">
          {title}
        </h2>
        <EmptyState title="No events yet">
          <p>{emptyText}</p>
        </EmptyState>
      </section>
    );
  }

  const selected = rows.find((row): row is InboxEvent => row.readable === true && row.eventId === picked) ?? null;

  return (
    <section aria-labelledby={headingId}>
      <h2 id={headingId} className="op-section-title">
        {title} ({groupDigits(String(rows.length))})
      </h2>
      {moreAvailable ? (
        <p className="op-hint">Only the newest page is loaded here. Older events exist and are not shown on this screen.</p>
      ) : null}
      <div className="op-table-scroll">
        <Table aria-labelledby={headingId}>
          <TableCaption>{caption}</TableCaption>
          <TableHead>
            <TableRow>
              <TableHeaderCell>Received (UTC)</TableHeaderCell>
              <TableHeaderCell>Platform</TableHeaderCell>
              <TableHeaderCell>Kind</TableHeaderCell>
              <TableHeaderCell>Sender id, as given</TableHeaderCell>
              <TableHeaderCell>Text, as received</TableHeaderCell>
              <TableHeaderCell>Event time, as given</TableHeaderCell>
              <TableHeaderCell>State</TableHeaderCell>
              <TableHeaderCell>Marks</TableHeaderCell>
              <TableHeaderCell>Raw item</TableHeaderCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {rows.map((row, index) =>
              row.readable === true ? (
                <EventRow
                  key={`event:${row.eventId}`}
                  event={row}
                  selected={selected?.eventId === row.eventId}
                  onSelect={() => setPicked(row.eventId)}
                />
              ) : (
                <UnreadableTableRow key={`unreadable:${index}`} row={row} />
              ),
            )}
          </TableBody>
        </Table>
      </div>
      <p role="status" aria-live="polite" className="op-hint">
        {selected === null ? "" : `Showing the raw item of event ${selected.eventId}.`}
      </p>
      {selected === null ? null : (
        <RawItem
          key={selected.eventId}
          source={source}
          rev={rev}
          scopeId={scopeId}
          eventId={selected.eventId}
          idPrefix={headingId}
        />
      )}
    </section>
  );
}

function EventRow({ event, selected, onSelect }: { event: InboxEvent; selected: boolean; onSelect: () => void }) {
  const state = inboxStateText(event.state);
  return (
    <TableRow>
      <TableCell rowHeader className="op-mono">
        {instantLabel(event.receivedAtUtc, "UTC")}
      </TableCell>
      <TableCell>{platformName(event.platform)}</TableCell>
      <TableCell>{inboxKindText(event.kind)}</TableCell>
      <TableCell className="op-mono">{event.senderId ?? "No sender id given"}</TableCell>
      <TableCell>{event.text === null ? "No text in this event" : <span className="op-inbox__text">{event.text}</span>}</TableCell>
      <TableCell className="op-mono">{event.eventTimeAsGiven ?? "Not given"}</TableCell>
      <TableCell>
        <Badge tone={state.tone}>{state.text}</Badge>
        {event.stateBy !== null && event.stateAtUtc !== null ? (
          <div>
            Set by {event.stateBy} at {instantLabel(event.stateAtUtc, "UTC")}
          </div>
        ) : null}
        {event.stateNote === "" ? null : <div>Note: {event.stateNote}</div>}
      </TableCell>
      <TableCell>
        {event.unmatched ? (
          <p>
            <span className="op-mark">Not matched</span> {unmatchedText(event.unmatchedReason)}
          </p>
        ) : null}
        {event.oversize ? (
          <p>
            <span className="op-mark">Cut</span> A field was cut to its bound. The raw item keeps all of it.
          </p>
        ) : null}
        {event.unmatched || event.oversize ? null : <p>None</p>}
      </TableCell>
      <TableCell>
        <Button
          size="sm"
          variant="secondary"
          aria-pressed={selected}
          aria-label={`Show the raw item of event ${event.eventId}`}
          onClick={onSelect}
        >
          {selected ? "Showing" : "Show"}
        </Button>
      </TableCell>
    </TableRow>
  );
}

/** A row the desktop cannot read: shown with its reason and its raw JSON, never dropped. */
function UnreadableTableRow({ row }: { row: UnreadableRow }) {
  return (
    <TableRow>
      <TableCell colSpan={9}>
        <p>This row could not be read: {row.reason}. It is kept here with its raw JSON.</p>
        <pre className="op-raw" tabIndex={0} aria-label="Raw JSON of a row that could not be read">
          {row.rawJson}
        </pre>
      </TableCell>
    </TableRow>
  );
}

/** The raw item of the selected event, as the daemon kept it: untrusted text in a read-only, wrapping block. */
function RawItem({
  source,
  rev,
  scopeId,
  eventId,
  idPrefix,
}: {
  source: DataSource;
  rev: number;
  scopeId: string | null;
  eventId: string;
  idPrefix: string;
}) {
  const raw = useAsync<InboxRaw | null>(() => source.getInboxEvent(scopeId, eventId), [source, rev, scopeId, eventId]);
  const titleId = `${idPrefix}-raw`;

  if (raw.state.status === "error") {
    return <LoadError message="Could not read the raw item from the daemon." onRetry={raw.reload} />;
  }
  if (raw.state.status === "loading") {
    return <SkeletonRows label="Loading the raw item" rows={2} />;
  }

  const item = raw.state.value;
  if (item === null) return <p className="op-hint">No raw item is stored for this event in this channel.</p>;

  return (
    <section aria-labelledby={titleId}>
      <h3 id={titleId} className="op-section-title">
        Raw item, as received
      </h3>
      <p className="op-hint">
        Written by the sender. Shown as plain text in a read-only block. Nothing in it is a link or markup.
      </p>
      <pre className="op-raw" tabIndex={0} aria-labelledby={titleId}>
        {item.rawJson}
      </pre>
    </section>
  );
}

function unmatchedText(reason: string | null): string {
  if (reason === "no_account") return "No stored account has this recipient id.";
  if (reason === "ambiguous_account") {
    return "More than one stored account has this recipient id, so none was chosen.";
  }
  return reason === null ? "Reason not stated." : `Reason as stored: ${reason}`;
}
