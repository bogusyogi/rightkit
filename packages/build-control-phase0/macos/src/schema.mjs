export const REPORT_VERSION = 1;
export const STATUSES = new Set(['PASS', 'FAIL', 'BLOCKED_EXTERNAL', 'PENDING_USER_APPROVAL']);

export function assertReport(report) {
  if (!report || report.schema !== 'rightkit.build-control-phase0.macos.report/v1') throw new TypeError('invalid report schema');
  if (!STATUSES.has(report.status)) throw new TypeError('invalid report status');
  for (const key of ['generatedAt', 'machine', 'gates', 'evidence']) if (!(key in report)) throw new TypeError(`missing ${key}`);
  if (!Array.isArray(report.gates) || !Array.isArray(report.evidence)) throw new TypeError('gates/evidence must be arrays');
  return report;
}

export function gate(id, status, detail, evidence = []) {
  if (!STATUSES.has(status)) throw new TypeError(`invalid gate status: ${status}`);
  return { id, status, detail, evidence };
}
