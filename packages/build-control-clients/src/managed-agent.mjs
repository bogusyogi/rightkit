#!/usr/bin/env node
import { createManagedAgentRequest, sendManagedAgentRequest } from './index.mjs';

const write = (value, stream) => { if (typeof value === 'string' && value) stream.write(value); };
const argv0 = process.env.RIGHTKIT_MANAGED_AGENT_SHIM ?? process.argv[1];
try {
  const message = createManagedAgentRequest({ argv0, argv: process.argv.slice(2), workspace: process.cwd(), environment: process.env });
  const result = await sendManagedAgentRequest(message, {
    onEvent(event) {
      if (event.type === 'STDOUT') write(event.data, process.stdout);
      if (event.type === 'STDERR') write(event.data, process.stderr);
    }
  });
  write(result.stdout, process.stdout); write(result.stderr, process.stderr);
  if (result.exitCode !== 0 && result.error) process.stderr.write(`rightkit managed-agent: ${result.error}\n`);
  process.exitCode = result.exitCode;
} catch (error) {
  process.stderr.write(`rightkit managed-agent: ${error.message}\n`);
  process.exitCode = 125;
}
