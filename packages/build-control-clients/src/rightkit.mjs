#!/usr/bin/env node
import { createRightkitRequest, sendManagedAgentRequest } from './index.mjs';

const write = (value, stream) => { if (typeof value === 'string' && value) stream.write(value); };
try {
  const message = createRightkitRequest({ argv: process.argv.slice(2), workspace: process.cwd(), environment: process.env });
  const result = await sendManagedAgentRequest(message, { onEvent(event) { if (event.type === 'STDOUT') write(event.data, process.stdout); if (event.type === 'STDERR') write(event.data, process.stderr); } });
  if (result.exitCode !== 0 && result.error) process.stderr.write(`rightkit: ${result.error}\n`);
  process.exitCode = result.exitCode;
} catch (error) {
  process.stderr.write(`rightkit: ${error.message}\n`);
  process.exitCode = 125;
}
