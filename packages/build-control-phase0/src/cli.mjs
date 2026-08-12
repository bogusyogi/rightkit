#!/usr/bin/env node
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { adapterNames, runAdapter } from './adapters.mjs';
import { capacity } from './capacity.mjs';
import { inventory } from './inventory.mjs';
import { reportStatus } from './types.mjs';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../../../../');
const outputIndex = process.argv.indexOf('--output'); const requestedOutput = outputIndex >= 0 ? process.argv[outputIndex + 1] : null;
const output = requestedOutput ? (isAbsolute(requestedOutput) ? requestedOutput : resolve(repositoryRoot, requestedOutput)) : null;
const selected = process.argv.includes('--adapter') ? [process.argv[process.argv.indexOf('--adapter') + 1]] : adapterNames();
const results = selected.map(runAdapter);
const machineInventory = inventory(); const capacityModel = capacity();
const report = { schemaVersion: '1.0.0', kind: 'rightkit-build-control-phase0', generatedAt: new Date().toISOString(), status: reportStatus(results), inventory: machineInventory, capacity: capacityModel, adapters: results };
if (output) {
  const evidenceDirectory = dirname(output); const platformName = machineInventory.host.platform === 'darwin' ? 'macos' : machineInventory.host.platform;
  mkdirSync(evidenceDirectory, { recursive: true }); writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
  writeFileSync(resolve(evidenceDirectory, `machine-inventory-${platformName}.json`), `${JSON.stringify(machineInventory, null, 2)}\n`);
  writeFileSync(resolve(evidenceDirectory, `capacity-model-${platformName}-${machineInventory.host.arch}.json`), `${JSON.stringify(capacityModel, null, 2)}\n`);
}
process.stdout.write(`${JSON.stringify({ status: report.status, adapters: results.map(({ adapter, status }) => ({ adapter, status })), output }, null, 2)}\n`);
process.exitCode = report.status === 'FAIL' ? 1 : 0;
