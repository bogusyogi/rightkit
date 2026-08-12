import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createFixture } from './fixtures.mjs';
import { commandExists, passed, run } from './process.mjs';
import { ADAPTERS, RESULT } from './types.mjs';

const definitions = {
  cargo: { tool: 'cargo', commands: [['cargo',['build']],['cargo',['build']],['cargo',['test']],['cargo',['run']]] },
  swiftpm: { tool: 'swift', commands: [['swift',['build']],['swift',['build']],['swift',['test']],['swift',['run']]] },
  'node-pnpm': { tool: 'pnpm', commands: [['pnpm',['run','build']],['pnpm',['run','build']],['pnpm',['run','test']],['pnpm',['run','generated']]] },
  'node-gyp': { tool: 'pnpm', commands: [['pnpm',['install','--ignore-scripts']],['pnpm',['run','rebuild']],['pnpm',['run','test']],['pnpm',['run','generated']]] },
  cmake: { tool: 'cmake', commands: [['cmake',['-S','.','-B','build']],['cmake',['--build','build']],['ctest',['--test-dir','build','--output-on-failure']],['./build/phase0',[]]] },
  make: { tool: 'make', commands: [['make',['clean','all']],['make',['all']],['make',['test']],['make',['generated']]] },
  'raw-compiler': { tool: 'cc', commands: [['cc',['-o','phase0','main.c']],['cc',['-o','phase0','main.c']],['./phase0',[]],['./phase0',[]]] }
};
const labels = ['clean', 'incremental', 'test', 'generatedExecutable'];

export function adapterNames() { return ADAPTERS; }
export function runAdapter(name) {
  const spec = definitions[name]; if (!spec) throw new Error(`unknown adapter: ${name}`);
  if (!commandExists(spec.tool)) return { adapter: name, status: RESULT.SKIPPED, reason: `required tool unavailable: ${spec.tool}`, steps: [] };
  if (name === 'cmake' && !commandExists('ctest')) return { adapter: name, status: RESULT.SKIPPED, reason: 'required tool unavailable: ctest', steps: [] };
  const root = mkdtempSync(join(tmpdir(), `rightkit-phase0-${name}-`));
  try {
    createFixture(name, root);
    const steps = spec.commands.map(([command, args], index) => ({ phase: labels[index], ...run(command, args, root) }));
    const failed = steps.find((step) => !passed(step));
    return { adapter: name, status: failed ? RESULT.FAIL : RESULT.PASS, fixture: root, steps, reason: failed ? `${failed.phase} failed` : undefined };
  } finally { rmSync(root, { recursive: true, force: true }); }
}
