#!/usr/bin/env node
import { cancelManagedAgentJob, createRightkitRequest, loadRuntimeIdentity, statusManagedAgentJob, submitAndWatchManagedAgentJob, watchManagedAgentJob } from './index.mjs';

const write = (value, stream) => { if (typeof value === 'string' && value) stream.write(value); };
const jobEvent = (event) => event.type === 'JOB_EVENT' ? event.event : event;
const streamJobEvent = (event) => { const value = jobEvent(event); if (value.type === 'STDOUT') write(value.data, process.stdout); if (value.type === 'STDERR') write(value.data, process.stderr); };
try {
  const argv = process.argv.slice(2); const runtimeIdentity = await loadRuntimeIdentity();
  if (['status', 'attach', 'cancel'].includes(argv[0])) {
    if (argv.length !== 2) throw new TypeError(`usage: rightkit ${argv[0]} <request-id>`);
    const operation = argv[0] === 'status' ? statusManagedAgentJob : argv[0] === 'attach' ? watchManagedAgentJob : cancelManagedAgentJob;
    const result = await operation(argv[1], { runtimeIdentity, onEvent: streamJobEvent });
    process.stdout.write(`${JSON.stringify(result)}\n`); process.exitCode = result.exitCode ?? 0;
  } else {
    const message = createRightkitRequest({ argv, workspace: process.cwd(), environment: process.env, runtimeIdentity });
    let requestId; let cancelling;
    const cancel = (signal) => {
      if (!requestId || cancelling) return;
      process.stderr.write(`rightkit: ${signal} cancelling request ${requestId}\n`);
      cancelling = cancelManagedAgentJob(requestId, { runtimeIdentity, source: 'CLIENT_SIGNAL' })
        .then((receipt) => process.stderr.write(`rightkit: cancel ${JSON.stringify(receipt)}\n`))
        .catch((error) => process.stderr.write(`rightkit: cancel failed for ${requestId}: ${error.message}\n`));
    };
    const result = await submitAndWatchManagedAgentJob(message, {
      onEvent: streamJobEvent,
      onSubmitted(submitted) {
        requestId = submitted.id;
        process.stderr.write(`rightkit: request ${requestId}\n`);
        process.once('SIGINT', () => cancel('SIGINT'));
        process.once('SIGTERM', () => cancel('SIGTERM'));
      }
    });
    await cancelling;
    if (result.exitCode !== 0 && result.error) process.stderr.write(`rightkit: ${result.error}\n`);
    process.exitCode = result.exitCode;
  }
} catch (error) {
  process.stderr.write(`rightkit: ${error.message}\n`);
  process.exitCode = 125;
}
