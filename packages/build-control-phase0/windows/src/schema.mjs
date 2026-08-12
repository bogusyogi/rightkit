export const REPORT_SCHEMA = "rightkit.build-control.windows.phase0.report.v1";
export const OUTCOMES = new Set(["PASS", "FAIL", "BLOCKED_EXTERNAL"]);

export function report({ outcome, generatedAt, machine, probes, blockers = [], notes = [] }) {
  if (!OUTCOMES.has(outcome)) throw new TypeError(`invalid outcome: ${outcome}`);
  if (!machine || typeof machine !== "object") throw new TypeError("machine is required");
  if (!Array.isArray(probes) || !Array.isArray(blockers)) throw new TypeError("probes and blockers must be arrays");
  if (outcome === "BLOCKED_EXTERNAL" && blockers.length === 0) throw new TypeError("blocked report requires blocker");
  return { schema: REPORT_SCHEMA, outcome, generatedAt, machine, probes, blockers, notes };
}

export function probe(name, status, detail, evidence = {}) {
  if (!/^[a-z0-9-]+$/.test(name)) throw new TypeError("probe name must be kebab case");
  if (!OUTCOMES.has(status)) throw new TypeError(`invalid probe status: ${status}`);
  return { name, status, detail, evidence };
}

export function blocker(id, dependency, requiredFor) {
  return { id, status: "BLOCKED_EXTERNAL", dependency, requiredFor };
}
