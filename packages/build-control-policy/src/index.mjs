const tools = new Set(['cargo', 'rustc', 'rustdoc']);
const basename = (value) => value.replace(/\\/g, '/').split('/').pop().replace(/\.(?:exe|cmd)$/i, '').toLowerCase();
const trustedAgentPath = (value) => /(?:\.rightkit-managed|rightkitmanagedagent)[\\/]agent-bin/i.test(value);
const packageManagerInvocation = (command) => {
  if (!/^(?:env\s+(?:[A-Za-z_][A-Za-z0-9_]*=[^\s]+\s+)*)?(?:pnpm|npm)(?:\.cmd)?(?:\s|$)/i.test(command) || /[;&|$()`<>\n]/.test(command)) return false;
  const pathAssignments = [...command.matchAll(/(?:^|\s)PATH=([^\s]+)/gi)];
  return pathAssignments.every((match) => trustedAgentPath(match[1]));
};

export function managedBuildViolation(command) {
  if (typeof command !== 'string') throw new TypeError('command must be a string');
  const trimmed = command.trim();
  // RightKit is permitted only as one uncomposed request: no chained shell,
  // redirect, substitution, or nested command can piggyback on its allow.
  if (/^rightkit(?:\s|$)/.test(trimmed) && !/[;&|$()`<>\n]/.test(trimmed)) return null;
  // The managed shims are reached through PATH, so redirecting PATH shadows
  // them and every later check. packageManagerInvocation already refused such a
  // command, but refusing it only fell through to a scan that does not list the
  // package managers, so the redirect was silently permitted.
  if ([...trimmed.matchAll(/(?:^|\s)PATH=([^\s]+)/gi)].some((match) => !trustedAgentPath(match[1]))) return 'path-redirect';
  // Package managers remain available. Their Cargo children inherit the
  // managed Cargo shim from PATH; denying their command line would prevent
  // that routing while also blocking non-Rust package-manager work.
  if (packageManagerInvocation(trimmed)) return null;
  if (/\brustup\s+run\b[^;&|()\n]*\b(cargo|rustc|rustdoc)\b/i.test(command)) return 'rustup-run';
  if (/\balias\s+[A-Za-z_][A-Za-z0-9_]*=.?[^\s;&|()]*\b(?:cargo|rustc|rustdoc)\b/i.test(command)) return 'alias';
  for (const word of command.matchAll(/(?:^|[\s;&|()$`"'])(\/?[^\s;&|()$`"']+)/g)) { const name = basename(word[1]); if (tools.has(name)) return name; }
  return null;
}

export function claudeHookDecision(input) {
  const command = input?.tool_input?.command ?? input?.command;
  const blocked = managedBuildViolation(command ?? '');
  return blocked ? { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: `[HOOK:rightkit-managed-build:block]\nWhy: Direct managed build tool forbidden: ${blocked}\nRequired: Use exact rightkit entrypoint.` } } : { decision: 'allow' };
}

export function clineHookDecision(input) {
  const command = input?.preToolUse?.parameters?.command ?? input?.tool_input?.command ?? input?.command;
  const blocked = managedBuildViolation(command ?? '');
  return blocked
    ? { cancel: true, errorMessage: `[HOOK:rightkit-managed-build:block]\nWhy: Direct managed build tool forbidden: ${blocked}\nRequired: Use exact rightkit entrypoint.` }
    : { cancel: false };
}
