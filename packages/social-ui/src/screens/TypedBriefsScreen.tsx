import { useState } from "react";
import { EmptyState, Field, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect.js";
import { LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import {
  groupDigits,
  instantLabel,
  type BriefVariant,
  type DataSource,
  type DemoStep,
  type ExplainerBeats,
  type OperatorScope,
  type SceneSpec,
  type TypedBrief,
} from "../data/index.js";
import { BRIEF_TYPE_LABEL, LINK_PLACEMENT_LABEL, platformName } from "./labels.js";

interface Props {
  source: DataSource;
  rev: number;
}

/** The explainer's five beats in story order. Each key is the beat's own key on ExplainerBeats. */
const EXPLAINER_BEATS: readonly { key: keyof ExplainerBeats; title: string }[] = [
  { key: "question", title: "Question" },
  { key: "model", title: "Model" },
  { key: "proof", title: "Proof" },
  { key: "turn", title: "Turn" },
  { key: "payoff", title: "Payoff" },
];

export function TypedBriefsScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Typed briefs"
      subtitle="Read only. The typed briefs of one channel, the platform variants made from each brief. Nothing here writes a brief or derives a variant."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Typed briefs are kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's briefs or variants. */}
          <BriefsView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function BriefsView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const { state, reload, value } = useAsync<TypedBrief[]>(
    () => source.listTypedBriefs(scopeId),
    [source, rev, scopeId],
  );
  const [picked, setPicked] = useState<string | null>(null);

  if (state.status === "error") {
    return <LoadError message="Could not read the typed briefs from the daemon." onRetry={reload} />;
  }
  if (value === null) return <SkeletonRows label="Loading typed briefs" rows={3} />;
  if (value.length === 0) {
    return (
      <EmptyState title="No typed briefs yet">
        <p>This channel has no typed brief. A brief is written on the daemon host or with the CLI, not here. This is not an error.</p>
      </EmptyState>
    );
  }

  const selected = value.find((b) => b.briefId === picked) ?? value[0];

  return (
    <>
      <section aria-labelledby="typed-list-title">
        <h2 id="typed-list-title" className="op-section-title">
          Typed briefs ({groupDigits(String(value.length))})
        </h2>
        <div className="op-table-scroll">
          <Table aria-labelledby="typed-list-title">
            <TableCaption>Typed briefs in this channel, by brief id</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Brief id</TableHeaderCell>
                <TableHeaderCell>Type</TableHeaderCell>
                <TableHeaderCell>Hook</TableHeaderCell>
                <TableHeaderCell align="end">
                  Revision
                </TableHeaderCell>
                <TableHeaderCell>Last changed (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {value.map((brief) => (
                <TableRow key={brief.briefId}>
                  <TableCell rowHeader className="op-mono">
                    {brief.briefId}
                  </TableCell>
                  <TableCell>{BRIEF_TYPE_LABEL[brief.briefType]}</TableCell>
                  <TableCell>{brief.hook}</TableCell>
                  <TableCell numeric>{groupDigits(String(brief.revision))}</TableCell>
                  <TableCell className="op-mono">{instantLabel(brief.updatedAtUtc, "UTC")}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </section>

      <section aria-labelledby="typed-detail-heading">
        <h2 id="typed-detail-heading" className="op-section-title">
          Selected typed brief
        </h2>
        <Field label="Typed brief">
          {({ id, "aria-describedby": describedBy }) => (
            <select
              id={id}
              aria-describedby={describedBy}
              className="op-select"
              value={selected.briefId}
              onChange={(e) => setPicked(e.target.value)}
            >
              {value.map((brief) => (
                <option key={brief.briefId} value={brief.briefId}>
                  {brief.briefId} ({BRIEF_TYPE_LABEL[brief.briefType]})
                </option>
              ))}
            </select>
          )}
        </Field>
        {/* Keyed by brief so a switch never shows the previous brief's parts or variants. */}
        <TypedBriefDetail key={selected.briefId} source={source} rev={rev} scopeId={scopeId} briefId={selected.briefId} />
      </section>
    </>
  );
}

function TypedBriefDetail({
  source,
  rev,
  scopeId,
  briefId,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  briefId: string;
}) {
  const brief = useAsync<TypedBrief | null>(() => source.getTypedBrief(scopeId, briefId), [source, rev, scopeId, briefId]);
  const variants = useAsync<BriefVariant[]>(
    () => source.listBriefVariants(scopeId, briefId),
    [source, rev, scopeId, briefId],
  );

  if (brief.state.status === "error") {
    return <LoadError message="Could not read this typed brief from the daemon." onRetry={brief.reload} />;
  }
  if (variants.state.status === "error") {
    return <LoadError message="Could not read the platform variants from the daemon." onRetry={variants.reload} />;
  }
  if (brief.state.status === "loading" || variants.state.status === "loading") {
    return <SkeletonRows label="Loading the typed brief" rows={3} />;
  }

  const record = brief.state.value;
  if (record === null) return <p className="op-hint">This typed brief is no longer in this channel.</p>;
  const derived = variants.state.value;

  return (
    <article className="op-detail" aria-labelledby="typed-detail-title">
      <h3 id="typed-detail-title" className="op-detail__title">
        {record.briefId}
      </h3>

      <dl className="op-dl">
        <dt>Brief id</dt>
        <dd className="op-mono">{record.briefId}</dd>
        <dt>Type</dt>
        <dd>{BRIEF_TYPE_LABEL[record.briefType]}</dd>
        <dt>Revision</dt>
        <dd>{groupDigits(String(record.revision))}</dd>
        <dt>Created (UTC)</dt>
        <dd>{instantLabel(record.createdAtUtc, "UTC")}</dd>
        <dt>Last changed (UTC)</dt>
        <dd>{instantLabel(record.updatedAtUtc, "UTC")}</dd>
      </dl>

      <h4 className="op-section-title">Hook</h4>
      <div className="op-caption">{record.hook}</div>

      <h4 className="op-section-title">Body</h4>
      <div className="op-caption">{record.body}</div>

      {record.briefType === "explainer" ? <ExplainerParts beats={record.explainer} /> : null}
      {record.briefType === "product_demo" ? <DemoSteps steps={record.demoSteps} /> : null}

      <VariantSection variants={derived} currentRevision={record.revision} />
    </article>
  );
}

/** The explainer's five beats, each with its scene spec. A beat the engine lacks is never invented. */
function ExplainerParts({ beats }: { beats: ExplainerBeats | null }) {
  if (beats === null) {
    return (
      <section aria-labelledby="typed-beats-title">
        <h4 id="typed-beats-title" className="op-section-title">
          Explainer beats
        </h4>
        <p className="op-hint">The engine holds no beats for this explainer.</p>
      </section>
    );
  }
  const scenes: ExplainerBeats = beats;
  return (
    <section aria-labelledby="typed-beats-title">
      <h4 id="typed-beats-title" className="op-section-title">
        Explainer beats
      </h4>
      <ol className="op-id-list">
        {EXPLAINER_BEATS.map((beat, index) => (
          <li key={beat.key}>
            <p>
              <strong>
                Beat {groupDigits(String(index + 1))}: {beat.title}
              </strong>
            </p>
            <SceneSpecView scene={scenes[beat.key]} />
          </li>
        ))}
      </ol>
    </section>
  );
}

function SceneSpecView({ scene }: { scene: SceneSpec }) {
  return (
    <dl className="op-dl">
      <dt>Teaches</dt>
      <dd>{scene.teaches}</dd>
      <dt>Frame</dt>
      <dd>{scene.frame}</dd>
      <dt>Motion</dt>
      <dd>{scene.motion}</dd>
      <dt>Words</dt>
      <dd>{scene.words}</dd>
      <dt>Sound</dt>
      <dd>{scene.sound}</dd>
    </dl>
  );
}

/** The product demo's steps, in the order they are recorded. Each step is the text said during it. */
function DemoSteps({ steps }: { steps: readonly DemoStep[] }) {
  return (
    <section aria-labelledby="typed-steps-title">
      <h4 id="typed-steps-title" className="op-section-title">
        Demo steps
      </h4>
      {steps.length === 0 ? (
        <p className="op-hint">The engine holds no demo steps for this brief.</p>
      ) : (
        <ol className="op-id-list">
          {steps.map((step, index) => (
            <li key={`step-${index}`}>{step.say}</li>
          ))}
        </ol>
      )}
    </section>
  );
}

/** The platform variants of one brief, with the revision and rule revision each was made under. */
function VariantSection({ variants, currentRevision }: { variants: BriefVariant[]; currentRevision: number }) {
  const older = variants.filter((variant) => variant.briefRevision < currentRevision).length;

  return (
    <section aria-labelledby="typed-variants-title">
      <h4 id="typed-variants-title" className="op-section-title">
        Platform variants
      </h4>
      <p className="op-hint">
        A variant is one platform rendering of this brief, made under one platform rule revision. The engine keeps each
        variant as it was made: an edit to the brief or a new rule revision makes a new variant rather than changing this one.
      </p>
      <p className="op-hint">
        The engine stores no separate hashtag list, and keeps no duration on a variant. Hashtags appear in the caption
        text as stored. The duration is checked against the rule when a variant is made, and is not shown here.
      </p>
      {older > 0 ? (
        <p className="op-hint">
          {groupDigits(String(older))} {older === 1 ? "variant was" : "variants were"} made from an older revision of this
          brief. The brief is now at revision {groupDigits(String(currentRevision))}.
        </p>
      ) : null}

      {variants.length === 0 ? (
        <p className="op-hint">No platform variant is stored for this brief yet.</p>
      ) : (
        <div className="op-table-scroll">
          <Table aria-labelledby="typed-variants-title">
            <TableCaption>Platform variants of this brief, by platform</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Variant id</TableHeaderCell>
                <TableHeaderCell>Platform</TableHeaderCell>
                <TableHeaderCell>Caption, as stored</TableHeaderCell>
                <TableHeaderCell>Link placement</TableHeaderCell>
                <TableHeaderCell>Link</TableHeaderCell>
                <TableHeaderCell align="end">
                  Brief revision
                </TableHeaderCell>
                <TableHeaderCell align="end">
                  Rule revision
                </TableHeaderCell>
                <TableHeaderCell>Created (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {variants.map((variant) => (
                <TableRow key={variant.variantId}>
                  <TableCell rowHeader className="op-mono">
                    {variant.variantId}
                  </TableCell>
                  <TableCell>{platformName(variant.platform)}</TableCell>
                  <TableCell>
                    <pre
                      className="op-pre"
                      tabIndex={0}
                      aria-label={`Caption of the ${platformName(variant.platform)} variant ${variant.variantId} as stored`}
                    >
                      {variant.caption}
                    </pre>
                  </TableCell>
                  <TableCell>{LINK_PLACEMENT_LABEL[variant.linkPlacement]}</TableCell>
                  <TableCell>{variant.link === null ? "No link given" : <span className="op-mono">{variant.link}</span>}</TableCell>
                  <TableCell numeric>{groupDigits(String(variant.briefRevision))}</TableCell>
                  <TableCell numeric>{variant.ruleRevision}</TableCell>
                  <TableCell className="op-mono">{instantLabel(variant.createdAtUtc, "UTC")}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </section>
  );
}
