#!/usr/bin/env node
import { cancelManagedAgentJob, createManagedAgentRequest, loadRuntimeIdentity, submitAndWatchManagedAgentJob } from './index.mjs';

const write = (value, stream) => { if (typeof value === 'string' && value) stream.write(value); };
const jobEvent = (event) => event.type === 'JOB_EVENT' ? event.event : event;
const argv0 = process.env.RIGHTKIT_MANAGED_AGENT_SHIM ?? process.argv[1];
try {
  const message = createManagedAgentRequest({ argv0, argv: process.argv.slice(2), workspace: process.cwd(), environment: process.env, runtimeIdentity: await loadRuntimeIdentity() });
  let requestId; let cancelling;
  const cancel = (signal) => {
    if (!requestId || cancelling) return;
    process.stderr.write(`rightkit managed-agent: ${signal} cancelling request ${requestId}\n`);
    cancelling = cancelManagedAgentJob(requestId, { runtimeIdentity: message.runtime, source: 'CLIENT_SIGNAL' })
      .then((receipt) => process.stderr.write(`rightkit managed-agent: cancel ${JSON.stringify(receipt)}\n`))
      .catch((error) => process.stderr.write(`rightkit managed-agent: cancel failed for ${requestId}: ${error.message}\n`));
  };
  const result = await submitAndWatchManagedAgentJob(message, {
    onSubmitted(submitted) {
      requestId = submitted.id;
      process.once('SIGINT', () => cancel('SIGINT'));
      process.once('SIGTERM', () => cancel('SIGTERM'));
      process.stderr.write(`rightkit managed-agent: request ${requestId}\n`);
    },
    onEvent(event) {
      const value = jobEvent(event);
      if (value.type === 'STDOUT') write(value.data, process.stdout);
      if (value.type === 'STDERR') write(value.data, process.stderr);
    }
  });
  await cancelling;
  write(result.stdout, process.stdout); write(result.stderr, process.stderr);
  if (result.exitCode !== 0 && result.error) process.stderr.write(`rightkit managed-agent: ${result.error}\n`);
  process.exitCode = result.exitCode;
} catch (error) {
  process.stderr.write(`rightkit managed-agent: ${error.message}\n`);
  process.exitCode = 125;
}
