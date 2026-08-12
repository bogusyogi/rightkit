import path from 'node:path';
import { writeEvidence } from './phase0.mjs';

if (process.argv[2] !== 'probe') throw new Error('usage: node src/cli.mjs probe [evidence-directory]');
const output = path.resolve(process.argv[3] ?? '../../../docs/plans/evidence/rightkit-build-control-plane-v5/macos');
const workspace = path.resolve(process.env.RIGHTKIT_PHASE0_WORKSPACE ?? '../../../../../');
const report = await writeEvidence(output, workspace);
process.stdout.write(`${JSON.stringify({ status: report.status, output })}\n`);
