export const RESULT = Object.freeze({ PASS: 'PASS', FAIL: 'FAIL', SKIPPED: 'SKIPPED_TOOL_UNAVAILABLE', BLOCKED: 'BLOCKED_EXTERNAL' });
export const ADAPTERS = Object.freeze(['cargo', 'swiftpm', 'node-pnpm', 'node-gyp', 'cmake', 'make', 'raw-compiler']);

export function reportStatus(results) {
  if (results.some((entry) => entry.status === RESULT.FAIL)) return RESULT.FAIL;
  if (results.some((entry) => entry.status === RESULT.BLOCKED || entry.status === RESULT.SKIPPED)) return RESULT.BLOCKED;
  return RESULT.PASS;
}
