// GB-30 (audit gate-broker-2026-10-30): the gate contract's requirements carry stable ids, every id is indexed once, every link to the broker contract resolves in the broker
// contract's own index (pinned in test-support/broker-contract-ids.json), and the field and capability names the contract states are the ones the code exports. A tag with no
// row, a row with no tag, a duplicate id, a dangling link (the `D4`, `GR16`, `GR32` and `E48` of the audit) or a retired field required by the text is a failing test.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { NATIVE_ADAPTER_CAPABILITIES } from "./native-adapters.mjs";
import { RETIRED_RESULT_FIELDS } from "./provenance-verify.mjs";
import { SNAPSHOT_EVIDENCE_FIELDS } from "./snapshot-request.mjs";
import { REQUIRED_BROKER_CAPABILITIES } from "./bin/rightkit-gate.mjs";

const read = (rel) => fs.readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");
const doc = read("../../docs/GATE-CONTRACT.md");
const broker = JSON.parse(read("./test-support/broker-contract-ids.json"));
const INDEX_AT = doc.indexOf("### Requirement index");
const RESOLUTIONS_AT = doc.indexOf("## 11. Contract resolutions");
const body = doc.slice(0, RESOLUTIONS_AT);
const section11 = doc.slice(RESOLUTIONS_AT);
const indexRows = section11.slice(section11.indexOf("### Requirement index")).split("\n").filter((l) => /^\| GC-/.test(l)).map((l) => l.split("|").slice(1, -1).map((c) => c.trim()));
const brokerIds = new Set([...broker.decisions, ...broker.acceptance, ...Object.keys(broker.requirements)]);

test("[GB-30] every requirement tag in the text has exactly one index row and every row has exactly one tag: no orphan, no duplicate", () => {
  assert.ok(INDEX_AT > 0 && RESOLUTIONS_AT > 0, "the contract has its index and resolutions");
  const tags = [...body.matchAll(/\[(GC-[ATIQL]\d+)\]/g)].map((m) => m[1]);
  const defined = [...body.matchAll(/(?:^|\n)(?:- |\d\. |\*\*[^*\n]+\*\* |### [^\n]*? |)\[(GC-[ATIQL]\d+)\]/g)].map((m) => m[1]);
  const rows = indexRows.map((r) => r[0]);
  assert.equal(new Set(rows).size, rows.length, `duplicate index row: ${rows.filter((id, i) => rows.indexOf(id) !== i)}`);
  // A tag may be cited again elsewhere (a resolution says "[GC-A05]"), but each id is DEFINED once, at the requirement itself.
  const definitions = new Map();
  for (const id of defined) definitions.set(id, (definitions.get(id) ?? 0) + 1);
  for (const id of rows) assert.equal(definitions.get(id), 1, `${id} is defined ${definitions.get(id) ?? 0} times in the text (once is required)`);
  for (const id of new Set(tags)) assert.ok(rows.includes(id), `${id} is tagged in the text but has no index row`);
  for (const id of definitions.keys()) assert.ok(rows.includes(id), `${id} is defined in the text but has no index row`);
  // Ids are contiguous per family, so a deleted requirement leaves a visible hole rather than a silent renumbering.
  for (const family of ["A", "T", "I", "Q", "L"]) {
    const numbers = rows.filter((id) => id.startsWith(`GC-${family}`)).map((id) => Number(id.slice(4)));
    assert.deepEqual(numbers, numbers.map((_, i) => i + 1), `GC-${family} ids are contiguous from 1`);
  }
  // Every row has a status the contract defines and a subject.
  for (const [id, subject, status] of indexRows) { assert.ok(subject.length > 3, id); assert.match(status, /^(operative|superseded|history)$/, id); }
});

test("[GB-30] every link to the broker contract resolves in the broker contract's own index (the audit's D4, GR16, GR32 and E48 cannot come back)", () => {
  assert.equal(broker.schema, "rightkit.broker-contract-ids/1");
  assert.match(broker.source.sha256, /^[0-9a-f]{64}$/); assert.match(broker.source.commit, /^[0-9a-f]{40}$/);
  for (const [id, , , links] of indexRows) {
    if (links === "-") continue;
    for (const link of links.split(",").map((l) => l.trim())) {
      const section = /^§(\d+)$/.exec(link);
      if (section) assert.ok(broker.sections.includes(Number(section[1])), `${id}: BROKER-CONTRACT has no section ${link}`);
      else assert.ok(brokerIds.has(link), `${id}: ${link} is not an id of the broker contract`);
    }
  }
  // Links in the prose (outside the resolutions table, which names the dangling ids on purpose to record their repair).
  const prose = [...body.matchAll(/\bBROKER-CONTRACT(?:\.md)?[ ,]+(BR\d+|B\d+|D\d+)\b|\bbroker contract's (D\d+|B\d+|BR\d+)\b|\b(BR\d+)\b/g)].map((m) => m[1] ?? m[2] ?? m[3]);
  assert.ok(prose.length >= 3, "the prose links to the broker contract");
  for (const link of prose) assert.ok(brokerIds.has(link), `the text links ${link}, which is not an id of the broker contract`);
  for (const dangling of ["GR16", "GR32", "E48"]) assert.equal(new RegExp(`\\b${dangling}\\b`).test(body), false, `${dangling} is a dangling reference`);
  assert.equal(/broker contract's D4\b/.test(body), false, "D4 does not exist (the broker's decisions are D1 to D3)");
  // The acceptance ids the text names (E1..E19) are rows of its own table, none beyond it.
  const rowsE = new Set([...doc.matchAll(/^\| (E\d+) /gm)].map((m) => m[1]));
  for (const n of [...body.matchAll(/\bE(\d+)\b/g)].map((m) => Number(m[1]))) assert.ok(rowsE.has(`E${n}`), `E${n} is referenced but has no acceptance row`);
  assert.deepEqual([...rowsE].map((e) => Number(e.slice(1))), Array.from({ length: rowsE.size }, (_, i) => i + 1), "the acceptance rows are contiguous");
});

test("[GB-30] the field and capability names the contract states are the code's: evidence fields, retired fields, required capabilities, adapter capabilities", () => {
  const returned = body.slice(body.indexOf("**Returned evidence**"), body.indexOf("**Broker lease.**"));
  // Every field the gate requires is a row of the returned-evidence table; no required field is marked retired.
  for (const field of SNAPSHOT_EVIDENCE_FIELDS) assert.ok(new RegExp(`\`${field.replace(".", "\\.")}[\`.]`).test(returned) || returned.includes(`\`${field}\``), `${field} is a returned-evidence row`);
  for (const field of RETIRED_RESULT_FIELDS) {
    assert.equal(SNAPSHOT_EVIDENCE_FIELDS.includes(field), false, `${field} is retired, never required`);
    const row = returned.split("\n").find((l) => l.startsWith(`| \`${field}\``));
    assert.ok(row && /Retired/.test(row), `the ${field} row says it is retired`);
  }
  // The capabilities: the contract names what the gate requires and says the stage grant is not one.
  assert.deepEqual([...REQUIRED_BROKER_CAPABILITIES], ["snapshot-readonly-v1", "snapshot-adapter-v1"]);
  for (const capability of REQUIRED_BROKER_CAPABILITIES) assert.ok(section11.includes(`\`${capability}\``), `${capability} is stated`);
  assert.match(section11, /`stage-grant-v1` is supported by the broker and is not a gate requirement/);
  assert.equal(REQUIRED_BROKER_CAPABILITIES.includes("stage-grant-v1"), false);
  for (const capability of NATIVE_ADAPTER_CAPABILITIES) assert.ok(doc.includes(`\`${capability.replace("adapter:", "")}\``), `${capability}'s adapter id is in the native-adapter table`);
  assert.ok(doc.includes("`external:cargo-ndk`") && doc.includes("`journeys-prebuilt-input`"));
});

test("[GB-30] the boundary table no longer gives the broker Class A verdict reuse (resolution G1)", () => {
  const broker = body.split("\n").find((l) => l.startsWith("| Broker | Mac/Dell execution"));
  const gate = body.split("\n").find((l) => l.startsWith("| Gate (this document)"));
  assert.ok(broker && gate);
  assert.equal(/verdict reuse/i.test(broker), false, "the broker row does not own Class A verdict reuse");
  assert.match(gate, /Class A ordering, inventory & verdict reuse/);
});

test("[GB-29] the Linux clauses name what the code implements: the direct-origin schema, the refused broker fields, the declared-target key, the held release code", async () => {
  const { BROKER_ONLY_FIELDS, DIRECT_ORIGIN_SCHEMA } = await import("./direct-origin.mjs");
  const clause = body.slice(body.indexOf("### Linux direct origin trust"), body.indexOf("### Clippy route"));
  assert.ok(clause.includes(`\`${DIRECT_ORIGIN_SCHEMA}\``), "the schema is named");
  for (const field of ["verdict", "refreshed", "provenance", "signature", "payload", "jobId", "requestId", "brokerVersion", "nonce"]) {
    assert.ok(BROKER_ONLY_FIELDS.includes(field), `${field} is refused by the code`);
    assert.ok(clause.includes(`\`${field}\``), `${field} is named as refused`);
  }
  assert.ok(clause.includes("`targets.linux`") && clause.includes("`RK_DIRECT_RELEASE_HELD`") && clause.includes("`NOT_APPLICABLE`"));
  assert.equal(/\bOPEN\b/.test(body.slice(body.indexOf("| Open decision"), body.indexOf("### Boundary with the RustRight"))) && /Direct Linux origin trust[^|]*\|[^|]*\|[^|]*OPEN/.test(body), false, "the Linux items are no longer open");
});
