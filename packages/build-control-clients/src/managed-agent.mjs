#!/usr/bin/env node
import { createManagedAgentRequest, loadRuntimeIdentity, submitAndWatchManagedAgentJob } from './index.mjs';

const write = (value, stream) => { if (typeof value === 'string' && value) stream.write(value); };
const jobEvent = (event) => event.type === 'JOB_EVENT' ? event.event : event;
const argv0 = process.env.RIGHTKIT_MANAGED_AGENT_SHIM ?? process.argv[1];
try {
  const message = createManagedAgentRequest({ argv0, argv: process.argv.slice(2), workspace: process.cwd(), environment: process.env, runtimeIdentity: await loadRuntimeIdentity() });
  const result = await submitAndWatchManagedAgentJob(message, {
    onEvent(event) {
      const value = jobEvent(event);
      if (value.type === 'STDOUT') write(value.data, process.stdout);
      if (value.type === 'STDERR') write(value.data, process.stderr);
    }
  });
  write(result.stdout, process.stdout); write(result.stderr, process.stderr);
  if (result.exitCode !== 0 && result.error) process.stderr.write(`rightkit managed-agent: ${result.error}\n`);
  process.exitCode = result.exitCode;
} catch (error) {
  process.stderr.write(`rightkit managed-agent: ${error.message}\n`);
  process.exitCode = 125;
}
