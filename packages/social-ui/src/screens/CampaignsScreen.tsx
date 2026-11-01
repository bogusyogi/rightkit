import { useState } from "react";
import { EmptyState, Field, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect";
import { LoadError, MockBanner } from "../components/Shared";
import { useAsync } from "../hooks/useAsync";
import {
  groupDigits,
  instantLabel,
  type Campaign,
  type CampaignAttribution,
  type DataSource,
  type OperatorScope,
  type TrackedLink,
} from "../data";
import { platformName } from "./labels";

interface Props {
  source: DataSource;
  rev: number;
}

/** A visit count as exact digits, grouped for reading. A missing count reads as not reported. */
function visitText(count: number | undefined): string {
  return count === undefined ? "Not reported" : groupDigits(String(count));
}

export function CampaignsScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Campaigns"
      subtitle="Read only. The campaigns of one channel, their tracked links and the visits recorded against those links. Nothing here creates a link, records a visit or redirects a visitor."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Campaigns are kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's campaigns. */}
          <CampaignView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function CampaignView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const { state, reload, value } = useAsync<Campaign[]>(
    () => source.listCampaigns(scopeId),
    [source, rev, scopeId],
  );
  const [picked, setPicked] = useState<string | null>(null);

  if (state.status === "error") {
    return <LoadError message="Could not read the campaigns from the daemon." onRetry={reload} />;
  }
  if (value === null) return <SkeletonRows label="Loading campaigns" rows={3} />;
  if (value.length === 0) {
    return (
      <EmptyState title="No campaigns yet">
        <p>
          This channel has no campaign. A campaign is made on the daemon host, not here. This is not an error.
        </p>
      </EmptyState>
    );
  }

  const selected = value.find((c) => c.id === picked) ?? value[0];

  return (
    <>
      <section aria-labelledby="campaigns-list-title">
        <h2 id="campaigns-list-title" className="op-section-title">
          Campaigns ({value.length})
        </h2>
        <div className="op-table-scroll">
          <Table aria-labelledby="campaigns-list-title">
            <TableCaption>Campaigns in this channel</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Campaign</TableHeaderCell>
                <TableHeaderCell>Campaign id</TableHeaderCell>
                <TableHeaderCell>utm_campaign</TableHeaderCell>
                <TableHeaderCell align="end">
                  Revision
                </TableHeaderCell>
                <TableHeaderCell>Last changed (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {value.map((campaign) => (
                <TableRow key={campaign.id}>
                  <TableCell rowHeader>{campaign.name}</TableCell>
                  <TableCell className="op-mono">{campaign.id}</TableCell>
                  <TableCell className="op-mono">{campaign.utmCampaign}</TableCell>
                  <TableCell numeric>{campaign.revision}</TableCell>
                  <TableCell className="op-mono">{instantLabel(campaign.updatedAtUtc, "UTC")}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </section>

      <section aria-labelledby="campaign-links-title">
        <h2 id="campaign-links-title" className="op-section-title">
          Tracked links and visits
        </h2>
        <Field label="Campaign">
          {({ id, "aria-describedby": describedBy }) => (
            <select
              id={id}
              aria-describedby={describedBy}
              className="op-select"
              value={selected.id}
              onChange={(e) => setPicked(e.target.value)}
            >
              {value.map((campaign) => (
                <option key={campaign.id} value={campaign.id}>
                  {campaign.name}
                </option>
              ))}
            </select>
          )}
        </Field>
        {/* Keyed by campaign so a switch never shows the previous campaign's links or counts. */}
        <CampaignLinks key={selected.id} source={source} rev={rev} scopeId={scopeId} campaign={selected} />
      </section>
    </>
  );
}

function CampaignLinks({
  source,
  rev,
  scopeId,
  campaign,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  campaign: Campaign;
}) {
  const links = useAsync<TrackedLink[]>(
    () => source.listCampaignLinks(scopeId, campaign.id),
    [source, rev, scopeId, campaign.id],
  );
  const attribution = useAsync<CampaignAttribution | null>(
    () => source.campaignAttribution(scopeId, campaign.id),
    [source, rev, scopeId, campaign.id],
  );

  const linkState = links.state;
  const countState = attribution.state;
  if (linkState.status === "error") {
    return <LoadError message="Could not read the tracked links from the daemon." onRetry={links.reload} />;
  }
  if (countState.status === "error") {
    return <LoadError message="Could not read the visit counts from the daemon." onRetry={attribution.reload} />;
  }
  if (linkState.status === "loading" || countState.status === "loading") {
    return <SkeletonRows label="Loading the links and the visit counts" rows={3} />;
  }
  const linkList = linkState.value;
  const counts = countState.value;
  const visitsByLink = new Map((counts?.links ?? []).map((row) => [row.linkId, row.visits] as const));

  return (
    <>
      {counts === null ? (
        <p className="op-hint">The engine holds no visit counts for this campaign in this channel.</p>
      ) : (
        <p className="op-hint">
          Total visits across this campaign&apos;s links: {groupDigits(String(counts.totalVisits))}. These are the
          counts the daemon holds. A link with zero visits is a normal state: no visit has been recorded for it yet.
          The desktop records no visits.
        </p>
      )}

      {linkList.length === 0 ? (
        <p className="op-hint">
          This campaign has no tracked links yet. Links are cut on the daemon host, not here. This is not an error.
        </p>
      ) : (
        <div className="op-table-scroll">
          <Table aria-labelledby="campaign-links-title">
            <TableCaption>Tracked links of {campaign.name}</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Link</TableHeaderCell>
                <TableHeaderCell>Variant</TableHeaderCell>
                <TableHeaderCell>Platform</TableHeaderCell>
                <TableHeaderCell>Final URL</TableHeaderCell>
                <TableHeaderCell>utm_source</TableHeaderCell>
                <TableHeaderCell>utm_medium</TableHeaderCell>
                <TableHeaderCell>utm_campaign</TableHeaderCell>
                <TableHeaderCell>utm_content</TableHeaderCell>
                <TableHeaderCell>Calendar entry</TableHeaderCell>
                <TableHeaderCell align="end">
                  Visits
                </TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {linkList.map((link) => (
                <TableRow key={link.id}>
                  <TableCell rowHeader className="op-mono">
                    {link.id}
                  </TableCell>
                  <TableCell>{link.variantId}</TableCell>
                  <TableCell>{platformName(link.platform)}</TableCell>
                  <TableCell className="op-mono">{link.finalUrl}</TableCell>
                  <TableCell className="op-mono">{link.utmSource}</TableCell>
                  <TableCell className="op-mono">{link.utmMedium}</TableCell>
                  <TableCell className="op-mono">{link.utmCampaign}</TableCell>
                  <TableCell className="op-mono">{link.utmContent}</TableCell>
                  <TableCell className="op-mono">{link.calendarEntryId ?? "None"}</TableCell>
                  <TableCell numeric>{visitText(visitsByLink.get(link.id))}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </>
  );
}
