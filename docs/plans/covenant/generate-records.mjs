import { readFileSync } from 'node:fs';
import { createCovenantRecord, digestValue } from '/Volumes/D/claude/tools/skills/legion/skills/covenant/lib/contracts.mjs';

const req1 = JSON.parse(readFileSync(new URL('./cargo-cache-round1-request.json', import.meta.url), 'utf8'));
const req2 = JSON.parse(readFileSync(new URL('./cargo-cache-round2-request.json', import.meta.url), 'utf8'));
const planV3 = readFileSync(new URL('../2026-08-12-machine-wide-cargo-cache-v3.md', import.meta.url), 'utf8');

const seats1 = [
  { seatId: 'cargo-semantics', lens: 'Cargo semantics & cache correctness', isolated: true, providerId: 'gpt-5.6-terra', reusedModel: true, position: 'REVISE' },
  { seatId: 'storage-reliability', lens: 'Storage reliability, concurrency & recovery', isolated: true, providerId: 'gpt-5.6-terra', reusedModel: true, position: 'REVISE' },
  { seatId: 'machine-enforcement', lens: 'Machine-wide enforcement & bypass resistance', isolated: true, providerId: 'gpt-5.6-terra', reusedModel: true, position: 'UNRESOLVED: packet content unavailable to seat' },
];
const findings1 = [
  ['F-1','cargo-semantics','Absolute Cargo, rustup, or scrubbed-environment invocation can bypass machine policy.','critical','IN_SCOPE_DEFECT'],
  ['F-2','cargo-semantics','Caller target directory inside managed root can remain unregistered.','high','IN_SCOPE_DEFECT'],
  ['F-3','cargo-semantics','Light commands, including fetch, can skip storage policy.','high','IN_SCOPE_DEFECT'],
  ['F-4','cargo-semantics','Cargo config precedence is not authoritatively parsed or rejected.','high','IN_SCOPE_DEFECT'],
  ['F-5','storage-reliability','Guard omits Cargo/Rustup homes, build-dir, sccache, & temp containment.','critical','IN_SCOPE_DEFECT'],
  ['F-6','storage-reliability','Targets lack process-safe per-entry leases.','high','IN_SCOPE_DEFECT'],
  ['F-7','storage-reliability','Path-first bootstrap can create internal fallback when D is absent.','critical','IN_SCOPE_DEFECT'],
  ['F-8','storage-reliability','GC is release-only, destructive, & can treat measurement errors as zero.','high','IN_SCOPE_DEFECT'],
  ['F-9','storage-reliability','PID-only leases are unsafe under PID reuse.','high','IN_SCOPE_DEFECT'],
  ['F-10','storage-reliability','Migration is narrow & can directly activate unvalidated moved state.','high','IN_SCOPE_DEFECT'],
  ['F-11','storage-reliability','PATH shim does not cover direct Cargo/rustup routes.','high','IN_SCOPE_DEFECT'],
  ['F-12','machine-enforcement','Seat lacked embedded review packet content.','high','MISSING_EVIDENCE'],
].map(([findingId,lens,statement,severity,classification])=>({findingId,lens,statement,severity,classification,confidence:0.95}));

const rec1 = createCovenantRecord(req1, {
  recordId: 'CV-1',
  seatRecords: seats1,
  synthesis: 'Round 1 requires revision: bootstrap, routing, leases, GC, migration, & bypass contract are incomplete; third seat lacked packet evidence.',
  findings: findings1,
  disagreements: [],
  missingEvidence: ['Machine-enforcement seat could not access packet body in round 1.'],
  risks: ['Internal-disk fallback', 'Unbounded cache growth', 'Concurrent deletion', 'Unvalidated migration'],
  outcome: 'REVISE',
  callerDispositions: findings1.map(f=>({findingId:f.findingId, disposition:f.findingId==='F-12'?'SUPERSEDED':'ACCEPT'})),
  revisionSubjectId: req2.subjectId,
  revisionDigest: digestValue(req2.embeddedEvidence),
  freshVerdictRecordId: 'CV-2',
  lane: 'decision-challenge',
  providerMetadata: { provider: 'multi-seat-agent-panel', degraded: false, failures: [], reusedModel: true },
  integrity: { digestVerified: true, mutationDetected: false },
});

const seats2 = [
  { seatId: 'cargo-semantics', lens: 'Cargo semantics & cache correctness', isolated: true, providerId: 'gpt-5.6-terra', reusedModel: true, position: 'REVISE' },
  { seatId: 'storage-reliability', lens: 'Storage reliability, concurrency & recovery', isolated: true, providerId: 'gpt-5.6-terra', reusedModel: true, position: 'REVISE' },
  { seatId: 'machine-enforcement', lens: 'Machine-wide enforcement & bypass resistance', isolated: true, providerId: 'gpt-5.6-terra', reusedModel: true, position: 'REVISE' },
];
const findings2 = [
  ['F-1','machine-enforcement','Absolute real Cargo with scrubbed environment remains outside policy despite broad machine-wide wording.','high','IN_SCOPE_DEFECT'],
  ['F-2','machine-enforcement','cargo install --root can redirect payload internally.','high','IN_SCOPE_DEFECT'],
  ['F-3','machine-enforcement','Executable Cargo aliases can shell out, scrub environment, & bypass guard.','medium','IN_SCOPE_DEFECT'],
  ['F-4','storage-reliability','Concurrent heavy builds lack global capacity reservation & admission control.','high','IN_SCOPE_DEFECT'],
  ['F-5','storage-reliability','Cargo descendants can write HOME, XDG, npm/pnpm, native-builder, or arbitrary internal paths.','high','IN_SCOPE_DEFECT'],
  ['F-6','storage-reliability','Cross-volume migration cannot use same-volume rename & lacks crash-safe copy protocol.','medium','IN_SCOPE_DEFECT'],
  ['F-7','cargo-semantics','Shared Cargo-home & sccache GC can race active consumers.','high','IN_SCOPE_DEFECT'],
  ['F-8','cargo-semantics','Free-space prediction alone cannot preserve floor under concurrency or underestimated growth.','high','IN_SCOPE_DEFECT'],
  ['F-9','cargo-semantics','Guard does not preserve every Cargo/rustup toolchain-selection route.','high','IN_SCOPE_DEFECT'],
].map(([findingId,lens,statement,severity,classification])=>({findingId,lens,statement,severity,classification,confidence:0.95}));

const rec2 = createCovenantRecord(req2, {
  recordId: 'CV-2',
  seatRecords: seats2,
  synthesis: 'Fresh round 2 still requires revision: operational scope, install/alias routing, global capacity, descendant caches, shared-cache maintenance, toolchain selection, & cross-volume migration need explicit contracts.',
  findings: findings2,
  disagreements: [],
  missingEvidence: [],
  risks: ['Policy bypass', 'Quota exhaustion under concurrency', 'Shared-cache race', 'Cross-volume data loss'],
  outcome: 'REVISE',
  callerDispositions: findings2.map(f=>({findingId:f.findingId, disposition:'ACCEPT'})),
  revisionSubjectId: 'machine-wide-cargo-cache-v3',
  revisionDigest: digestValue(planV3),
  freshVerdictRecordId: null,
  lane: 'fresh-verdict',
  providerMetadata: { provider: 'multi-seat-agent-panel', degraded: false, failures: [], reusedModel: true },
  integrity: { digestVerified: true, mutationDetected: false },
});

console.log(JSON.stringify({rec1,rec2}, null, 2));
