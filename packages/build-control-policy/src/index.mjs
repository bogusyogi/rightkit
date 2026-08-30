const tools = new Set(['cargo', 'rustc', 'rustdoc']);
const basename = (value) => value.replace(/\\/g, '/').split('/').pop().replace(/\.(?:exe|cmd)$/i, '').toLowerCase();
const trustedAgentPath = (value) => /(?:\.rightkit-managed|rightkitmanagedagent)[\\/]agent-bin/i.test(value);
const packageManagerInvocation = (command) => {
  if (!/^(?:env\s+(?:[A-Za-z_][A-Za-z0-9_]*=[^\s]+\s+)*)?(?:pnpm|npm)(?:\.cmd)?(?:\s|$)/i.test(command) || /[;&|$()`<>\n]/.test(command)) return false;
  const pathAssignments = [...command.matchAll(/(?:^|\s)PATH=([^\s]+)/gi)];
  return pathAssignments.every((match) => trustedAgentPath(match[1]));
};

function heredocOpeners(line) {
  const openers = [];
  let quote = null;
  for (let index = 0; index < line.length; index += 1) {
    const character = line[index];
    if (quote) {
      if (character === '\\' && quote === '"') index += 1;
      else if (character === quote) quote = null;
      continue;
    }
    if (character === '\\') { index += 1; continue; }
    if (character === '"' || character === "'") { quote = character; continue; }
    if (character !== '<' || line[index + 1] !== '<' || line[index + 2] === '<') continue;
    index += 2;
    const stripTabs = line[index] === '-'; if (stripTabs) index += 1;
    while (line[index] === ' ' || line[index] === '\t') index += 1;
    let delimiter = '';
    const delimiterQuote = line[index] === '"' || line[index] === "'" ? line[index++] : null;
    while (index < line.length) {
      const value = line[index];
      if (delimiterQuote ? value === delimiterQuote : /[\s;&|<>()]/.test(value)) break;
      if (value === '\\' && index + 1 < line.length) { index += 1; delimiter += line[index]; }
      else delimiter += value;
      index += 1;
    }
    if (delimiter) openers.push(Object.freeze({ delimiter, stripTabs }));
  }
  return openers;
}

// Shell heredoc bodies are stdin data, not executable shell text. Keep each
// command line (including the redirect itself) so a real `cargo <<EOF` still
// blocks, but remove body/delimiter lines before scanning tool tokens.
function withoutHeredocBodies(command) {
  const pending = []; const retained = [];
  for (const line of command.split('\n')) {
    if (pending.length) {
      const active = pending[0]; const candidate = active.stripTabs ? line.replace(/^\t+/, '') : line;
      if (candidate === active.delimiter) pending.shift();
      retained.push('');
      continue;
    }
    retained.push(line); pending.push(...heredocOpeners(line));
  }
  return retained.join('\n');
}

export function managedBuildViolation(command) {
  if (typeof command !== 'string') throw new TypeError('command must be a string');
  const executableText = withoutHeredocBodies(command);
  const trimmed = executableText.trim();
  // Sanctioned RightKit subcommands remain safe inside ordinary shell wrappers,
  // pipes, redirects, & cwd setup. Remove only their managed-tool token, then
  // scan every remaining executable token so appended bare Cargo still blocks.
  const withoutSanctionedRightkit = executableText.replace(/\brightkit\s+(cargo|rustc|rustdoc)\b/gi, (match, tool) => match.replace(tool, '__rightkit_managed_tool__'));
  // The managed shims are reached through PATH, so redirecting PATH shadows
  // them and every later check. packageManagerInvocation already refused such a
  // command, but refusing it only fell through to a scan that does not list the
  // package managers, so the redirect was silently permitted.
  if ([...trimmed.matchAll(/(?:^|\s)PATH=([^\s]+)/gi)].some((match) => !trustedAgentPath(match[1]))) return 'path-redirect';
  // Package managers remain available. Their Cargo children inherit the
  // managed Cargo shim from PATH; denying their command line would prevent
  // that routing while also blocking non-Rust package-manager work.
  if (packageManagerInvocation(trimmed)) return null;
  if (/\brustup\s+run\b[^;&|()\n]*\b(cargo|rustc|rustdoc)\b/i.test(withoutSanctionedRightkit)) return 'rustup-run';
  if (/\balias\s+[A-Za-z_][A-Za-z0-9_]*=.?[^\s;&|()]*\b(?:cargo|rustc|rustdoc)\b/i.test(withoutSanctionedRightkit)) return 'alias';
  // A quote character is not a command boundary the way `;`/`|` is — it
  // delimits a string argument, and text inside one is normally never
  // executed. The previous tokenizer treated every quote as a delimiter,
  // which tore open quoted text and re-scanned its contents; a single-word
  // quote broke on the open quote, but a multi-word one still leaked,
  // since ordinary spaces *inside* the quotes still looked like ordinary
  // token boundaries. `echo "the cargo team"` blocked on the bare word
  // "cargo" appearing inside a quoted, non-executed sentence.
  //
  // The one case where quoted text IS executed: a shell/interpreter flag
  // (`-c`, `-lc`, `/c`, `-Command`, ...) that hands its quoted argument to
  // a nested interpreter as a script, e.g. `zsh -lc "cargo build"`. Those
  // spans are left untouched so the scan still reaches inside them; every
  // other quoted span is stripped before the token scan ever sees it.
  const shellScriptFlag = /(?:^|[\s;&|()`])(?:-c|-lc|\/c|-command|-encodedcommand)\s*$/i;
  const withoutInertQuotedSpans = withoutSanctionedRightkit.replace(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g, (match, offset, full) => shellScriptFlag.test(full.slice(0, offset)) ? match : '""');
  for (const word of withoutInertQuotedSpans.matchAll(/(?:^|[\s;&|()$`"'])(\/?[^\s;&|()$`"']+)/g)) { const name = basename(word[1]); if (tools.has(name)) return name; }
  return null;
}

function denialMessage(blocked) { return `[HOOK:rightkit-managed-build:block]\nWhy: Direct managed build tool forbidden: ${blocked}\nRequired: Use exact rightkit entrypoint.`; }

export function claudeHookDecision(input) {
  const command = input?.tool_input?.command ?? input?.command;
  const blocked = managedBuildViolation(command ?? '');
  return blocked ? { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: denialMessage(blocked) } } : { decision: 'allow' };
}

export function clineHookDecision(input) {
  const command = input?.preToolUse?.parameters?.command ?? input?.tool_input?.command ?? input?.command;
  const blocked = managedBuildViolation(command ?? '');
  return blocked
    ? { cancel: true, errorMessage: denialMessage(blocked) }
    : { cancel: false };
}
