import { useState } from "react";
import { Button, EmptyState, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect";
import { LoadError, MockBanner } from "../components/Shared";
import { useAsync } from "../hooks/useAsync";
import { shell } from "../lib/shell";
import { groupDigits, instantLabel, type ChannelLink, type DataSource, type OperatorScope } from "../data";
import { platformName } from "./labels";
import { hostAppName } from "../appName";

interface Props {
  source: DataSource;
  rev: number;
}

type CopyFn = (text: string, what: string) => Promise<void>;

export function LinksScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Links"
      subtitle="Read only. The tracked links of one channel: each destination, its UTM parts and full tagged URL, the campaign and the post it belongs to, and the visit counts the daemon holds. Nothing here cuts a link, records a visit or redirects a visitor."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Links are kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's links. */}
          <LinksView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function LinksView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const links = useAsync<ChannelLink[]>(() => source.listChannelLinks(scopeId), [source, rev, scopeId]);
  const [notice, setNotice] = useState("");

  const copy: CopyFn = async (text, what) => {
    try {
      await shell.clipboard.write({ text });
      setNotice(`Copied ${what}.`);
    } catch {
      setNotice(`Copying ${what} failed. Select the text and copy it by hand.`);
    }
  };

  if (links.state.status === "error") {
    return <LoadError message="Could not read the tracked links from the daemon." onRetry={links.reload} />;
  }
  if (links.value === null) return <SkeletonRows label="Loading the tracked links" rows={3} />;

  const rows = links.value;
  if (rows.length === 0) {
    return (
      <EmptyState title="No links yet">
        <p>
          Nothing is wrong: no links have been recorded for this channel yet. Links are cut on the daemon host, not
          here.
        </p>
      </EmptyState>
    );
  }

  return (
    <>
      <p className="op-hint">
        {hostAppName()} records links; it does not count clicks here. A link is a string: nothing serves it or redirects a
        visitor. Visit counts are the plain numbers the daemon has recorded, shown exactly as stored. The engine gives
        a count no time and no source of its own, so each row says so. Nothing here computes a rate, a total or a
        ranking.
      </p>
      <p role="status" aria-live="polite" className="op-hint">
        {notice}
      </p>

      <section aria-labelledby="links-list-title">
        <h2 id="links-list-title" className="op-section-title">
          Tracked links ({groupDigits(String(rows.length))})
        </h2>
        <div className="op-table-scroll">
          <Table aria-labelledby="links-list-title">
            <TableCaption>
              Tracked links in this channel, by campaign, variant, platform and link id
            </TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Link</TableHeaderCell>
                <TableHeaderCell>Campaign</TableHeaderCell>
                <TableHeaderCell>Post (calendar entry)</TableHeaderCell>
                <TableHeaderCell>Destination</TableHeaderCell>
                <TableHeaderCell>UTM parts</TableHeaderCell>
                <TableHeaderCell>Full tagged URL</TableHeaderCell>
                <TableHeaderCell>State</TableHeaderCell>
                <TableHeaderCell>Created (UTC)</TableHeaderCell>
                <TableHeaderCell>Last changed</TableHeaderCell>
                <TableHeaderCell align="end">
                  Visits recorded
                </TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {rows.map((row) => (
                <LinkRow key={`link:${row.link.scopeId}:${row.link.id}`} row={row} copy={copy} />
              ))}
            </TableBody>
          </Table>
        </div>
      </section>
    </>
  );
}

/** One link. Each URL is text with its own Copy button; neither is an anchor, so nothing opens from here. */
function LinkRow({ row, copy }: { row: ChannelLink; copy: CopyFn }) {
  const { link } = row;
  const label = `${platformName(link.platform)}, variant ${link.variantId}, link ${link.id}`;
  const utmParts: readonly (readonly [string, string])[] = [
    ["utm_source", link.utmSource],
    ["utm_medium", link.utmMedium],
    ["utm_campaign", link.utmCampaign],
    ["utm_content", link.utmContent],
  ];

  return (
    <TableRow>
      <TableCell rowHeader>
        <div className="op-mono">{link.id}</div>
        <div>
          {platformName(link.platform)}, variant <span className="op-mono">{link.variantId}</span>
        </div>
      </TableCell>
      <TableCell>
        <div>{row.campaignName}</div>
        <div className="op-mono">{link.campaignId}</div>
      </TableCell>
      <TableCell className="op-mono">{link.calendarEntryId ?? "No calendar entry attached"}</TableCell>
      <TableCell>
        <div className="op-mono">{link.destination}</div>
        <Button
          size="sm"
          variant="secondary"
          aria-label={`Copy the destination of ${label}`}
          onClick={() => copy(link.destination, `the destination of ${label}`)}
        >
          Copy
        </Button>
      </TableCell>
      <TableCell>
        <ul className="op-id-list">
          {utmParts.map(([name, value]) => (
            <li key={name} className="op-mono">
              {name}: {value}
            </li>
          ))}
        </ul>
      </TableCell>
      <TableCell>
        <div className="op-mono">{link.finalUrl}</div>
        <Button
          size="sm"
          variant="secondary"
          aria-label={`Copy the full tagged URL of ${label}`}
          onClick={() => copy(link.finalUrl, `the full tagged URL of ${label}`)}
        >
          Copy
        </Button>
      </TableCell>
      <TableCell>Stored. Links are never edited, so there is no later state.</TableCell>
      <TableCell className="op-mono">{instantLabel(link.createdAtUtc, "UTC")}</TableCell>
      <TableCell>Never: a link is not edited after it is created.</TableCell>
      <TableCell numeric>
        <span className="op-mono">{row.visits ?? "Not read"}</span>
        <div className="op-hint">Source: link.attribution, the recorded visit count. Time: not stated by the engine.</div>
      </TableCell>
    </TableRow>
  );
}
