import test from 'node:test';
import assert from 'node:assert/strict';
import {
  alreadyRecordedNote,
  checkReconcileForm,
  conflictText,
  differenceRows,
  estimatedText,
  formatSignedUsd,
  formatUsd,
  invoiceReconciledText,
  isValidDay,
  isValidMonth,
  linesTotal,
  outcomeCounts,
  outcomeHeadline,
  parseInvoiceLine,
  parseInvoiceLines,
  parseUsd,
  periodBounds,
  providerReportedText,
  resolvePeriod,
  summarizeReconcile,
  unmatchedReasonText,
  utcDaysInMonth,
  utcMonthOf
} from '../views/spend-reconcile-logic.ts';
import { mockEngine } from '../lib/browser-mock-api.ts';

const blankAmounts = (over = {}) => ({
  rows: 0, estimated_usd: null, reserved_usd: 0,
  provider_reported_usd: null, provider_reported_rows: 0, provider_reported_available: false,
  invoice_reconciled_usd: null, invoice_reconciled_rows: 0,
  settled_usd: 0, open_reserved_usd: 0, cap_counted_usd: 0, ...over
});

test('a typed amount is a plain non-negative dollar number', () => {
  assert.equal(parseUsd('4.25'), 4.25);
  assert.equal(parseUsd(' 12 '), 12);
  assert.equal(parseUsd('.5'), 0.5);
  assert.equal(parseUsd('0'), 0);
  for (const bad of ['', '   ', '$1', '-1', '1e3', 'abc', '1,5', '1.2.3']) {
    assert.equal(parseUsd(bad), null, `"${bad}" must not parse`);
  }
});

test('dollars read exactly: two decimals, more only when the figure needs them', () => {
  assert.equal(formatUsd(12.5), '$12.50');
  assert.equal(formatUsd(3), '$3.00');
  assert.equal(formatUsd(0), '$0.00');
  assert.equal(formatUsd(0.0065), '$0.0065');
  assert.equal(formatUsd(0.0000001), '$0.0000001');
  assert.equal(formatUsd(Number.NaN), 'unknown');
});

test('signed differences show their sign and no sign at zero', () => {
  assert.equal(formatSignedUsd(1.2), '+$1.20');
  assert.equal(formatSignedUsd(-0.5), '-$0.50');
  assert.equal(formatSignedUsd(0), '$0.00');
});

test('the default month is the UTC month, not the local one', () => {
  // 23:30 on 31 December in UTC-5 is already 1 January in UTC.
  assert.equal(utcMonthOf(new Date('2026-12-31T23:30:00-05:00')), '2027-01');
  assert.equal(utcMonthOf(new Date('2026-09-15T12:00:00Z')), '2026-09');
});

test('month and day validity uses real UTC calendar dates', () => {
  assert.equal(isValidMonth('2026-09'), true);
  assert.equal(isValidMonth('2026-13'), false);
  assert.equal(isValidMonth('2026-00'), false);
  assert.equal(isValidMonth('2026-9'), false);
  assert.equal(isValidDay('2028-02-29'), true);
  assert.equal(isValidDay('2026-02-29'), false);
  assert.equal(isValidDay('2026-02-31'), false);
  assert.equal(isValidDay('2026-04-31'), false);
  assert.equal(isValidDay('2026-09-1'), false);
});

test('days in a UTC month include leap years', () => {
  assert.equal(utcDaysInMonth('2026-02'), 28);
  assert.equal(utcDaysInMonth('2028-02'), 29);
  assert.equal(utcDaysInMonth('2026-09'), 30);
  assert.equal(utcDaysInMonth('2026-12'), 31);
});

test('a month covers its first to last UTC day, inclusive', () => {
  assert.deepEqual(periodBounds('2026-09'), { from: '2026-09-01', to: '2026-09-30' });
  assert.deepEqual(periodBounds('2026-02'), { from: '2026-02-01', to: '2026-02-28' });
  assert.deepEqual(periodBounds({ from: '2026-09-03', to: '2026-09-09' }), { from: '2026-09-03', to: '2026-09-09' });
});

test('the picker resolves to the engine period, or says what is wrong', () => {
  assert.deepEqual(resolvePeriod({ mode: 'month', month: '2026-09' }), { ok: true, period: '2026-09' });
  assert.deepEqual(resolvePeriod({ mode: 'month', month: '2026-9' }), { ok: false, error: 'Pick a month (UTC).' });
  assert.deepEqual(resolvePeriod({ mode: 'dates', from: '2026-09-01', to: '2026-09-14' }), { ok: true, period: { from: '2026-09-01', to: '2026-09-14' } });
  assert.equal(resolvePeriod({ mode: 'dates', from: '2026-09-01', to: '' }).ok, false);
  assert.equal(resolvePeriod({ mode: 'dates', from: '2026-02-30', to: '2026-03-01' }).ok, false);
  assert.equal(resolvePeriod({ mode: 'dates', from: '2026-09-14', to: '2026-09-01' }).error, 'The end date is before the start date.');
});

test('a line with a job id or a provider job id parses to exactly one key', () => {
  assert.deepEqual(parseInvoiceLine('job-a, 1.50'), { ok: true, line: { job_id: 'job-a', usd: 1.5 } });
  assert.deepEqual(parseInvoiceLine('provider:fal-123, 2'), { ok: true, line: { provider_job_id: 'fal-123', usd: 2 } });
  assert.deepEqual(parseInvoiceLine('job:job-b, 0.25'), { ok: true, line: { job_id: 'job-b', usd: 0.25 } });
  assert.deepEqual(parseInvoiceLine('job-d, 4, 2'), { ok: true, line: { job_id: 'job-d', usd: 4, attempt: 2 } });
});

test('bad lines are reported with their own line numbers and never guessed', () => {
  const text = [
    'job id, amount',           // 1: a header row is not a line
    '',                         // 2: blank, skipped
    'job-a, 1.50',              // 3: good
    'provider:fal-123, 2',      // 4: good
    'job-b, $3',                // 5: no $ sign
    'job-c, 4, 0',              // 6: attempt must start at 1
    'job-d, 4, 2',              // 7: good
    ', 4',                      // 8: no id
    'provider: , 4',            // 9: prefix with no id
    'job e, 4',                 // 10: space inside the id
    'job-f',                    // 11: no amount
    'job-g, -2',                // 12: negative
    'job-h, 1, 2, 3'            // 13: too many columns
  ].join('\n');
  const parsed = parseInvoiceLines(text);
  assert.deepEqual(parsed.lines, [
    { job_id: 'job-a', usd: 1.5 },
    { provider_job_id: 'fal-123', usd: 2 },
    { job_id: 'job-d', usd: 4, attempt: 2 }
  ]);
  assert.deepEqual(parsed.problems.map((p) => p.lineNumber), [1, 5, 6, 8, 9, 10, 11, 12, 13]);
  assert.equal(parsed.problems[0].text, 'job id, amount');
  assert.match(parsed.problems[0].reason, /space/);
  assert.match(parsed.problems[1].reason, /plain dollar/);
  assert.match(parsed.problems[2].reason, /whole number from 1/);
});

test('CRLF pasted text reads the same as LF', () => {
  const parsed = parseInvoiceLines('job-a, 1\r\njob-b, 2\r\n');
  assert.equal(parsed.lines.length, 2);
  assert.equal(parsed.problems.length, 0);
});

test('lines total adds the amounts without float noise', () => {
  assert.equal(linesTotal([{ job_id: 'a', usd: 0.1 }, { job_id: 'b', usd: 0.2 }]), 0.3);
  assert.equal(linesTotal([]), 0);
});

const goodForm = (over = {}) => ({
  provider: 'fal',
  period: { mode: 'month', month: '2026-09' },
  total: '12.50',
  invoiceRef: '',
  linesText: '',
  ...over
});

test('a complete form gives the exact engine input, with no keys it does not send', () => {
  const checked = checkReconcileForm(goodForm());
  assert.equal(checked.ok, true);
  assert.deepEqual(checked.input, { period: '2026-09', provider: 'fal', invoice_total_usd: 12.5 });
  assert.equal('invoice_ref' in checked.input, false);
  assert.equal('lines' in checked.input, false);
});

test('a typed reference and lines are sent as typed', () => {
  const checked = checkReconcileForm(goodForm({
    period: { mode: 'dates', from: '2026-09-01', to: '2026-09-14' },
    invoiceRef: '  INV-7  ',
    linesText: 'job-a, 1.50\nprovider:fal-9, 2'
  }));
  assert.equal(checked.ok, true);
  assert.deepEqual(checked.input, {
    period: { from: '2026-09-01', to: '2026-09-14' },
    provider: 'fal',
    invoice_total_usd: 12.5,
    invoice_ref: 'INV-7',
    lines: [{ job_id: 'job-a', usd: 1.5 }, { provider_job_id: 'fal-9', usd: 2 }]
  });
});

test('every form problem comes back at once, with bad line numbers', () => {
  const checked = checkReconcileForm(goodForm({ provider: '  ', total: '', linesText: 'job-a, 1\nbad line' }));
  assert.equal(checked.ok, false);
  assert.deepEqual(checked.errors, [
    'Choose a provider.',
    'Enter the invoice total in dollars, 0 or more.',
    '1 line cannot be read. Fix it before continuing.'
  ]);
  assert.deepEqual(checked.lineProblems.map((p) => p.lineNumber), [2]);
});

test('an invoice with bad lines is refused, not sent without them', () => {
  const checked = checkReconcileForm(goodForm({ linesText: 'job-a, 1\njob-b, $2\njob-c, -3' }));
  assert.equal(checked.ok, false);
  assert.deepEqual(checked.lineProblems.map((p) => p.lineNumber), [2, 3]);
  assert.match(checked.errors[0], /2 lines cannot be read/);
});

test('the summary says what will be sent and what will not change', () => {
  const checked = checkReconcileForm(goodForm({ linesText: 'job-a, 1.50\njob-b, 2.00' }));
  assert.equal(checked.ok, true);
  const text = checked.summary.join('\n');
  assert.match(text, /^Provider: fal$/m);
  assert.match(text, /^Period: 2026-09, the whole UTC month \(2026-09-01 to 2026-09-30\)$/m);
  assert.match(text, /^Invoice total: \$12\.50$/m);
  assert.match(text, /none typed\. One is generated from the provider and period/);
  assert.match(text, /^Lines: 2 \(\$3\.50 in total\)/m);
  assert.match(text, /Not changed: spending limits, reserved budget, settled amounts/);
  assert.match(text, /Nothing is sent to a provider and nothing is spent\.$/m);
});

test('the summary of a dates period names inclusive UTC days', () => {
  const checked = checkReconcileForm(goodForm({ period: { mode: 'dates', from: '2026-09-03', to: '2026-09-09' }, invoiceRef: 'INV-9' }));
  const text = checked.summary.join('\n');
  assert.match(text, /^Period: 2026-09-03 to 2026-09-09, inclusive UTC days$/m);
  assert.match(text, /^Invoice reference: INV-9\. /m);
  assert.match(text, /^Lines: none\. Only the invoice total is recorded/m);
});

test('a result headline says recorded, or already recorded and nothing changed', () => {
  const base = {
    provider: 'fal', period: { from: '2026-09-01', to: '2026-09-30' }, already_recorded: false,
    lines: { given: 1, matched: [{ job_id: 'job-a', attempt: 1, invoice_usd: 1.5, settled_usd: 1.5, difference_usd: 0 }], unmatched_lines: [], unmatched_rows: [], lines_total_usd: 1.5 }
  };
  assert.equal(outcomeHeadline(base), 'Invoice recorded for fal, 2026-09-01 to 2026-09-30.');
  assert.equal(outcomeHeadline({ ...base, already_recorded: true }), 'This invoice was already recorded, nothing changed.');
  assert.equal(alreadyRecordedNote({ ...base, already_recorded: false }), null);
  assert.match(alreadyRecordedNote({ ...base, already_recorded: true }), /applied again safely/);
  assert.equal(alreadyRecordedNote({ ...base, already_recorded: true, lines: undefined }), null);
});

test('the counts separate matched lines, lines with no row and rows with no line', () => {
  const result = {
    lines: {
      given: 4,
      matched: [{}, {}],
      unmatched_lines: [
        { reason: 'no_ledger_row_for_provider_and_period' },
        { reason: 'duplicate_line_for_row' }
      ],
      unmatched_rows: [{}]
    }
  };
  assert.deepEqual(outcomeCounts(result), { matched: 2, noRow: 1, otherUnmatched: 1, rowsWithoutLine: 1 });
  assert.deepEqual(outcomeCounts({}), { matched: 0, noRow: 0, otherUnmatched: 0, rowsWithoutLine: 0 });
});

test('engine reasons read as plain text, unknown ones pass through', () => {
  assert.equal(unmatchedReasonText('no_ledger_row_for_provider_and_period'), 'no spend record for this provider in this period');
  assert.match(unmatchedReasonText('ambiguous_attempt: the job has several rows in the period; name the attempt'), /third column/);
  assert.equal(unmatchedReasonText('duplicate_line_for_row'), 'a second line for the same spend record');
  assert.match(unmatchedReasonText('row_already_reconciled_by_another_invoice_or_amount'), /not overwritten/);
  assert.equal(unmatchedReasonText('something_new'), 'something_new');
});

test('a reference conflict reads as text, with the engine detail after it', () => {
  const text = conflictText('invoice_ref INV-1 is already recorded for fal 2026-09-01..2026-09-30 with total $12.500000');
  assert.match(text, /^This invoice reference is already recorded with different details, so nothing was changed\./);
  assert.match(text, /Details: invoice reference INV-1 is already recorded/);
  assert.doesNotMatch(text, /invoice_ref/, 'the engine key name is not shown');
  assert.doesNotMatch(conflictText('   '), /Details:/);
});

test('differences show only the rows that can be compared', () => {
  const rows = differenceRows({
    invoice_total_minus_settled_usd: 1.2,
    settled_minus_estimated_usd: null,
    provider_reported_minus_settled_usd: null,
    invoice_reconciled_minus_settled_usd: -0.5,
    lines_total_minus_invoice_total_usd: 0
  });
  assert.deepEqual(rows.map((r) => r.value), ['+$1.20', '-$0.50', '$0.00']);
  assert.equal(rows[0].label, 'Invoice total minus settled spend, this period');
});

test('the Spend amount rows name the provider-reported and invoice-reconciled state', () => {
  assert.equal(providerReportedText(blankAmounts()), 'not reported by the provider');
  assert.equal(providerReportedText(blankAmounts({ provider_reported_usd: 0.4, provider_reported_rows: 1, provider_reported_available: true })), '$0.40 (1 row)');
  assert.equal(providerReportedText(blankAmounts({ provider_reported_usd: 0.9, provider_reported_rows: 3, provider_reported_available: true })), '$0.90 (3 rows)');
  assert.equal(invoiceReconciledText(blankAmounts()), 'not reconciled yet');
  assert.equal(invoiceReconciledText(blankAmounts({ invoice_reconciled_usd: 2, invoice_reconciled_rows: 2 })), '$2.00 (2 rows)');
  assert.equal(estimatedText(blankAmounts()), 'no estimate');
  assert.equal(estimatedText(blankAmounts({ estimated_usd: 0.0065 })), '$0.0065');
});

// The mock mirrors spend.reconcile: shapes, the already-recorded no-op, the conflict refusal, and
// that no cap or counted amount moves.

const command = (name, input) => mockEngine('command', { name, input });
const monthOfSeed = new Date(Date.now() - 3_600_000).toISOString().slice(0, 7);

test('mock reconcile records a matched line and leaves every cap and counted amount where it was', async () => {
  const before = await command('spend.status', {});
  const res = await command('spend.reconcile', {
    period: monthOfSeed,
    provider: 'fal',
    invoice_total_usd: 0.0065,
    invoice_ref: 'mock-inv-match',
    lines: [{ job_id: 'job-seed-1', usd: 0.0065 }]
  });
  assert.equal(res.already_recorded, false);
  assert.equal(res.invoice_ref_generated, false);
  assert.equal(res.lines.matched.length, 1);
  assert.equal(res.lines.matched[0].job_id, 'job-seed-1');
  assert.equal(res.lines.unmatched_lines.length, 0);
  assert.equal(res.lines.unmatched_rows.length, 0);
  assert.equal(res.invoice_reconciled_rows, 1);
  const after = await command('spend.status', {});
  for (const key of ['cap_usd', 'monthly_cap_usd', 'reserved_usd', 'actual_usd', 'month_counted_usd']) {
    assert.equal(after[key], before[key], `${key} must not move`);
  }
  assert.equal(after.amounts.month.invoice_reconciled_rows, 1);
});

test('mock reconcile reports a line with no row and a row with no line, never guessing', async () => {
  const res = await command('spend.reconcile', {
    period: monthOfSeed,
    provider: 'nanobanana',
    invoice_total_usd: 1,
    invoice_ref: 'mock-inv-nb',
    lines: [{ job_id: 'job-seed-1', usd: 1 }, { provider_job_id: 'nope', usd: 2 }]
  });
  assert.equal(res.lines.matched.length, 0);
  assert.equal(res.lines.unmatched_lines.length, 2);
  assert.equal(res.lines.unmatched_lines[0].reason, 'no_ledger_row_for_provider_and_period');
  assert.equal(res.lines.unmatched_rows.length, 1);
  assert.equal(res.lines.unmatched_rows[0].job_id, 'job-seed-2');
});

test('mock reconcile: the same reference and details is already recorded, nothing refused', async () => {
  const input = { period: monthOfSeed, provider: 'fal', invoice_total_usd: 3, invoice_ref: 'mock-inv-again' };
  const first = await command('spend.reconcile', input);
  assert.equal(first.already_recorded, false);
  const second = await command('spend.reconcile', input);
  assert.equal(second.already_recorded, true);
  assert.equal(second.invoice_total_usd, 3);
});

test('mock reconcile: the same reference with a different total is refused invoice_ref_conflict', async () => {
  await command('spend.reconcile', { period: monthOfSeed, provider: 'fal', invoice_total_usd: 5, invoice_ref: 'mock-inv-clash' });
  await assert.rejects(
    command('spend.reconcile', { period: monthOfSeed, provider: 'fal', invoice_total_usd: 6, invoice_ref: 'mock-inv-clash' }),
    (error) => error.code === 'invoice_ref_conflict' && /already recorded for fal/.test(error.message)
  );
});

test('mock reconcile refuses a malformed period and a missing provider as invalid_input', async () => {
  await assert.rejects(command('spend.reconcile', { period: '2026-13', provider: 'fal', invoice_total_usd: 1 }), (e) => e.code === 'invalid_input');
  await assert.rejects(command('spend.reconcile', { period: monthOfSeed, provider: '', invoice_total_usd: 1 }), (e) => e.code === 'invalid_input');
  await assert.rejects(command('spend.reconcile', { period: monthOfSeed, provider: 'fal', invoice_total_usd: -1 }), (e) => e.code === 'invalid_input');
});
