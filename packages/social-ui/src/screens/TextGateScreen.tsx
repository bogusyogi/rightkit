import { useState, type FormEvent } from "react";
import { Badge, Button, EmptyState, Field, Input, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect";
import { LoadError, MockBanner } from "../components/Shared";
import { useAsync } from "../hooks/useAsync";
import {
  formatMilli,
  groupDigits,
  instantLabel,
  type DataSource,
  type GateCheck,
  type GatePolicy,
  type GateRecord,
  type GateRequiredRules,
  type OperatorScope,
} from "../data";
import { GATE_DISPOSITION_LABEL, GATE_VERDICT_LABEL } from "./labels";

interface Props {
  source: DataSource;
  rev: number;
}

export function TextGateScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Text gate"
      subtitle="Read only. Checks a script against the channel's gate policy and shows each rule's verdict, then the stored verdict records of a subject. A check stores nothing. Nothing here changes the policy, records a check or approves anything."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>The text gate is kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's policy or check. */}
          <GateView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function GateView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const policy = useAsync<GatePolicy | null>(() => source.gatePolicy(scopeId), [source, rev, scopeId]);

  if (policy.state.status === "error") {
    return <LoadError message="Could not read the gate policy from the daemon." onRetry={policy.reload} />;
  }
  if (policy.state.status === "loading") return <SkeletonRows label="Loading the gate policy" rows={3} />;
  const current = policy.state.value;

  return (
    <>
      <section aria-labelledby="gate-policy-title">
        <h2 id="gate-policy-title" className="op-section-title">
          Gate policy
        </h2>
        {current === null ? (
          <p className="op-hint">No policy is set for this channel, so nothing can pass.</p>
        ) : (
          <PolicyView policy={current} />
        )}
      </section>

      <CheckSection source={source} scopeId={scopeId} />
      <RecordsSection source={source} rev={rev} scopeId={scopeId} />
    </>
  );
}

/** The current policy revision. Thresholds are written as decimals; the engine holds them in thousandths. */
function PolicyView({ policy }: { policy: GatePolicy }) {
  return (
    <dl className="op-dl">
      <dt>Revision</dt>
      <dd>{groupDigits(String(policy.revision))}</dd>
      <dt>Created (UTC)</dt>
      <dd>{instantLabel(policy.createdAtUtc, "UTC")}</dd>
      <dt>Banned phrases</dt>
      <dd>{listOrNone(policy.bannedPhrases)}</dd>
      <dt>Stock openers</dt>
      <dd>{listOrNone(policy.stockOpeners)}</dd>
      <dt>Minimum words</dt>
      <dd>{groupDigits(String(policy.minWords))}</dd>
      <dt>Dashes</dt>
      <dd>At most {formatMilli(policy.dashMaxPer100WordsMilli)} per 100 words</dd>
      <dt>Semicolons</dt>
      <dd>At most {formatMilli(policy.semicolonMaxPer100WordsMilli)} per 100 words</dd>
      <dt>Sentence length variance</dt>
      <dd>
        At least {formatMilli(policy.varianceMinMilli)}, inconclusive below{" "}
        {groupDigits(String(policy.varianceMinSentences))} sentences
      </dd>
      <dt>Tricolon sentences</dt>
      <dd>
        At most {formatMilli(policy.tricolonMaxPer100SentencesMilli)} per 100 sentences, inconclusive below{" "}
        {groupDigits(String(policy.tricolonMinSentences))} sentences
      </dd>
      <dt>Inconclusive or unverified blocks, for</dt>
      <dd>{requiredRules(policy.required)}</dd>
    </dl>
  );
}

/** A dry-run check of one script. The subject id labels the check; the check stores nothing. */
function CheckSection({ source, scopeId }: { source: DataSource; scopeId: string }) {
  const [subjectId, setSubjectId] = useState("");
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<GateCheck | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  async function runCheck(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (subjectId.trim() === "") {
      setResult(null);
      setProblem("Type a subject id first. Nothing was checked.");
      return;
    }
    if (text.trim() === "") {
      setResult(null);
      setProblem("Paste a script to check. Nothing was checked.");
      return;
    }
    setBusy(true);
    setProblem(null);
    const outcome = await source.checkText(scopeId, subjectId.trim(), text);
    setBusy(false);
    if (outcome.ok) {
      setResult(outcome.value);
    } else {
      setResult(null);
      setProblem(`The check did not run. ${outcome.error.message} Nothing was stored.`);
    }
  }

  return (
    <section aria-labelledby="gate-check-title">
      <h2 id="gate-check-title" className="op-section-title">
        Check a script
      </h2>
      <p className="op-hint">
        A script that contains a number will be blocked here, because this screen sends no claim annotations. That is
        the gate working, not an error.
      </p>
      <form className="op-form" onSubmit={runCheck}>
        <Field label="Subject id" note="The brief, hook or script this text belongs to. It labels the check and stores nothing.">
          {({ id, "aria-describedby": describedBy }) => (
            <Input
              id={id}
              aria-describedby={describedBy}
              type="text"
              value={subjectId}
              onChange={(e) => setSubjectId(e.target.value)}
            />
          )}
        </Field>
        <Field label="Script">
          {({ id, "aria-describedby": describedBy }) => (
            <textarea
              id={id}
              aria-describedby={describedBy}
              className="op-textarea"
              rows={8}
              value={text}
              onChange={(e) => setText(e.target.value)}
            />
          )}
        </Field>
        <div className="op-decide__buttons">
          <Button type="submit" variant="primary" disabled={busy}>
            Check
          </Button>
        </div>
      </form>

      {busy ? <p role="status" className="op-hint">Checking the script</p> : null}
      {problem !== null ? (
        <p role="alert" className="op-error-inline">
          {problem}
        </p>
      ) : null}
      <div aria-live="polite">{result !== null ? <CheckResult check={result} /> : null}</div>
    </section>
  );
}

/** The result of one check: the overall disposition in words, then one row per rule with the engine's reason. */
function CheckResult({ check }: { check: GateCheck }) {
  const disposition = GATE_DISPOSITION_LABEL[check.disposition];
  return (
    <article className="op-detail" aria-labelledby="gate-result-title">
      <h3 id="gate-result-title" className="op-detail__title">
        Result for {check.subjectId}
      </h3>
      <div className="op-badges">
        <Badge tone={disposition.tone}>{disposition.text}</Badge>
      </div>
      <dl className="op-dl">
        <dt>Policy revision</dt>
        <dd>{groupDigits(String(check.policyRevision))}</dd>
        <dt>Blocking rules</dt>
        <dd>{listOrNone(check.blockingRules)}</dd>
        <dt>Text hash</dt>
        <dd className="op-mono">{check.textHash}</dd>
        <dt>Stored</dt>
        <dd>Nothing. A check from this screen does not record.</dd>
      </dl>
      <div className="op-table-scroll">
        <Table>
          <TableCaption>Rules checked on this script</TableCaption>
          <TableHead>
            <TableRow>
              <TableHeaderCell>Rule</TableHeaderCell>
              <TableHeaderCell>Verdict</TableHeaderCell>
              <TableHeaderCell>Blocks</TableHeaderCell>
              <TableHeaderCell>Reason</TableHeaderCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {check.rules.map((rule) => (
              <TableRow key={rule.ruleId}>
                <TableCell rowHeader className="op-mono">
                  {rule.ruleId}
                </TableCell>
                <TableCell>{GATE_VERDICT_LABEL[rule.verdict]}</TableCell>
                <TableCell>{rule.blocks ? "Yes" : "No"}</TableCell>
                <TableCell>{rule.reason}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </article>
  );
}

/** The stored verdict records of one subject. Only recorded checks are stored, so a check from this screen never appears. */
function RecordsSection({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const [draft, setDraft] = useState("");
  const [subjectId, setSubjectId] = useState<string | null>(null);

  function showRecords(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const id = draft.trim();
    if (id !== "") setSubjectId(id);
  }

  return (
    <section aria-labelledby="gate-records-title">
      <h2 id="gate-records-title" className="op-section-title">
        Stored verdict records
      </h2>
      <p className="op-hint">Only checks the daemon recorded are stored. A check from this screen is never stored, so it never appears here.</p>
      <form className="op-form" onSubmit={showRecords}>
        <Field label="Subject id to read">
          {({ id, "aria-describedby": describedBy }) => (
            <Input
              id={id}
              aria-describedby={describedBy}
              type="text"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
            />
          )}
        </Field>
        <div className="op-decide__buttons">
          <Button type="submit" variant="secondary" disabled={draft.trim() === ""}>
            Show records
          </Button>
        </div>
      </form>
      {subjectId === null ? null : (
        <GateRecordsTable key={subjectId} source={source} rev={rev} scopeId={scopeId} subjectId={subjectId} />
      )}
    </section>
  );
}

function GateRecordsTable({
  source,
  rev,
  scopeId,
  subjectId,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  subjectId: string;
}) {
  const { state, reload, value } = useAsync<GateRecord[]>(
    () => source.gateRecords(scopeId, subjectId),
    [source, rev, scopeId, subjectId],
  );

  if (state.status === "error") {
    return <LoadError message="Could not read the stored records from the daemon." onRetry={reload} />;
  }
  if (value === null) return <SkeletonRows label="Loading the stored records" rows={2} />;
  if (value.length === 0) {
    return (
      <EmptyState title="No stored records for this subject">
        <p>Nothing has been recorded for {subjectId} in this channel. This is not an error.</p>
      </EmptyState>
    );
  }

  return (
    <div className="op-table-scroll">
      <Table>
        <TableCaption>Stored verdict records of {subjectId}, newest first</TableCaption>
        <TableHead>
          <TableRow>
            <TableHeaderCell align="end">
              Record
            </TableHeaderCell>
            <TableHeaderCell>Disposition</TableHeaderCell>
            <TableHeaderCell align="end">
              Policy revision
            </TableHeaderCell>
            <TableHeaderCell>Checked (UTC)</TableHeaderCell>
            <TableHeaderCell>Text hash</TableHeaderCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {value.map((record) => (
            <TableRow key={record.recordSeq}>
              <TableCell rowHeader numeric>
                {groupDigits(String(record.recordSeq))}
              </TableCell>
              <TableCell>{GATE_DISPOSITION_LABEL[record.disposition].text}</TableCell>
              <TableCell numeric>{groupDigits(String(record.policyRevision))}</TableCell>
              <TableCell className="op-mono">{instantLabel(record.checkedAtUtc, "UTC")}</TableCell>
              <TableCell className="op-mono">{record.textHash}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

function listOrNone(items: readonly string[]): string {
  return items.length === 0 ? "None" : items.join(", ");
}

/** The style rules whose inconclusive or unverified finding blocks. Named in words. */
function requiredRules(required: GateRequiredRules): string {
  const names: string[] = [];
  if (required.bannedPhrase) names.push("banned phrase");
  if (required.dashDensity) names.push("dash density");
  if (required.semicolonDensity) names.push("semicolon density");
  if (required.sentenceLengthVariance) names.push("sentence length variance");
  if (required.tricolonFrequency) names.push("tricolon frequency");
  if (required.stockOpener) names.push("stock opener");
  return listOrNone(names);
}
