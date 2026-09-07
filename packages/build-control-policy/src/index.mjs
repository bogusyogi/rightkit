import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, parse, resolve } from 'node:path';
import { localDevelopmentPolicy, localDevelopmentCargoAllowed } from '../../build-control-core/src/local-development.mjs';

const tools = new Set(['cargo', 'rustc', 'rustdoc']);
const githubActionsOnly = 'github-actions-only';
const dynamicOperation = /(?:^|[:_-])(?:compile|test|build|package|pack|sign|qualif(?:y|ication)|smoke|release|candidate|finalize)(?:$|[:_-])/i;
const shellScriptFlag = /(?:^|[\s;&|()`])(?:-c|-lc|\/c|-command|-encodedcommand)\s*$/i;
const staticPnpmInstall = /^pnpm(?:\.cmd)?\s+install\s+(?:(?:--frozen-lockfile|--lockfile-only|--ignore-scripts)\s*){2}$/i;
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

function directoryFrom(value) {
  const selected = resolve(value ?? process.cwd());
  try { return statSync(selected).isDirectory() ? selected : dirname(selected); }
  catch { return selected; }
}

function repositoryCompilePolicy(cwd) {
  let cursor = directoryFrom(cwd);
  const filesystemRoot = parse(cursor).root;
  for (;;) {
    const manifestPath = join(cursor, '.rightgit.json');
    const gitConfigPath = join(cursor, '.git', 'config');
    let orthicPublic = false;
    if (existsSync(gitConfigPath)) {
      try { orthicPublic = /github\.com(?::|\/)Orthic-Labs\//i.test(readFileSync(gitConfigPath, 'utf8')); }
      catch { orthicPublic = false; }
    }
    if (existsSync(manifestPath)) {
      try {
        const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
        return Object.freeze({ root: cursor, compile: manifest.compile ?? null, invalid: false, orthicPublic });
      } catch {
        return Object.freeze({ root: cursor, compile: null, invalid: true, orthicPublic });
      }
    }
    if (orthicPublic) return Object.freeze({ root: cursor, compile: null, invalid: false, orthicPublic: true });
    if (cursor === filesystemRoot) return Object.freeze({ root: null, compile: null, invalid: false, orthicPublic: false });
    cursor = dirname(cursor);
  }
}

function dynamicCommandOperation(command) {
  const executableText = withoutHeredocBodies(command);
  const trimmed = executableText.trim();
  if (staticPnpmInstall.test(trimmed)
    && (trimmed.includes('--frozen-lockfile') || trimmed.includes('--lockfile-only'))
    && trimmed.includes('--ignore-scripts')) return null;
  const withoutInertQuotedSpans = executableText.replace(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g, (match, offset, full) => shellScriptFlag.test(full.slice(0, offset)) ? match : '');
  const words = withoutInertQuotedSpans
    .split(/[\s;&|()$`<>]+/)
    .filter(Boolean)
    .map((word) => word.replace(/^['"]|['"]$/g, ''));
  for (let index = 0; index < words.length; index += 1) {
    const name = basename(words[index]);
    if (tools.has(name) || name === 'rustup') return name;
    if (name === 'rightkit' && ['cargo', 'rustc', 'rustdoc'].includes(words[index + 1]?.toLowerCase())) return words[index + 1].toLowerCase();
    if (name === 'right-release') {
      const operation = words[index + 1]?.toLowerCase();
      if (operation === 'addon' && words[index + 2]?.toLowerCase() === 'build') return 'addon-build';
      if (operation === 'publish' && ['cargo', 'swift'].includes(words[index + 2]?.toLowerCase())) return `publish-${words[index + 2].toLowerCase()}`;
      if (operation && (dynamicOperation.test(operation) || ['release', 'sign-windows', 'sign-updater', 'create-mac-updater', 'cargo-target'].includes(operation))) return operation;
    }
    if (['pnpm', 'npm', 'yarn'].includes(name)) {
      let candidate = words[index + 1]?.toLowerCase();
      if (['run', 'exec', 'dlx'].includes(candidate)) candidate = words[index + 2]?.toLowerCase();
      if (tools.has(basename(candidate ?? '')) || candidate === 'rustup') return basename(candidate);
      if (candidate === 'rightkit' && ['cargo', 'rustc', 'rustdoc'].includes(words[index + 3]?.toLowerCase())) return words[index + 3].toLowerCase();
      if (['install', 'ci', 'add', 'update', 'upgrade'].includes(candidate)) return 'install';
      if (candidate && dynamicOperation.test(candidate)) return candidate;
      const later = words.slice(index + 1, index + 5).find((token) => dynamicOperation.test(token.toLowerCase()));
      if (later) return later.toLowerCase();
    }
    if (name === 'node' && words.slice(index + 1, index + 4).some((token) => token === '--test' || dynamicOperation.test(basename(token)))) return 'test';
    if (dynamicOperation.test(name.replace(/\.(?:mjs|cjs|js|ts|sh|ps1|cmd|bat|exe)$/i, ''))) return name;
    if (['make', 'cmake', 'ninja', 'msbuild', 'xcodebuild', 'swiftc', 'tsc'].includes(name)) return name;
  }
  return null;
}

export function githubActionsOnlyViolation(command, { cwd = process.cwd(), environment = process.env, platform = process.platform } = {}) {
  if (typeof command !== 'string') throw new TypeError('command must be a string');
  const operation = dynamicCommandOperation(command);
  if (!operation || environment?.GITHUB_ACTIONS === 'true') return null;
  const policy = repositoryCompilePolicy(cwd);
  if (!policy.root) return null;
  if (policy.invalid) return `github-actions-only:invalid-policy:${operation}`;
  if (policy.compile === githubActionsOnly && localDevelopmentPolicy(policy.root, platform)
    && !/[;&|$()`<>\n]/.test(command)) {
    const text = command.trim();
    const hub = resolve(policy.root, 'apps/membrane-hub');
    if (resolve(cwd) === policy.root && /^pnpm(?:\.cmd)? --dir apps[\\/]membrane-hub run release:build:win:unsigned$/.test(text)) return null;
    if (resolve(cwd) === hub && (/^pnpm(?:\.cmd)? run release:build:win:unsigned$/.test(text)
      || /^node scripts[\\/]release-build-windows\.mjs --unsigned$/.test(text))) return null;
    const managed = /^(?:pnpm(?:\.cmd)? exec )?rightkit(?:\.cmd)? (cargo|rustc|rustdoc) (.+)$/.exec(text);
    if (managed) {
      const argv = managed[2].match(/"[^"]*"|'[^']*'|[^\s]+/g)?.map((arg) => arg.replace(/^["']|["']$/g, '')) ?? [];
      if (localDevelopmentCargoAllowed({ root: policy.root, workspace: cwd, operation: managed[1], argv, platform })) return null;
    }
  }
  if (policy.compile !== githubActionsOnly && !policy.orthicPublic) return `github-actions-only:missing-policy:${operation}`;
  return `github-actions-only:${operation}`;
}

export function managedBuildViolation(command) {
  if (typeof command !== 'string') throw new TypeError('command must be a string');
  const executableText = withoutHeredocBodies(command);
  const trimmed = executableText.trim();
  // Sanctioned RightKit subcommands remain safe inside ordinary shell wrappers,
  // pipes, redirects, & cwd setup. Remove only their managed-tool token, then
  // scan every remaining executable token so appended bare Cargo still blocks.
  const pathAssignments = [...executableText.matchAll(/(?:^|\s)PATH=([^\s;&|]+)/gi)];
  const rightkitCanResolveSafely = pathAssignments.every((match) => trustedAgentPath(match[1]));
  const withoutSanctionedRightkit = rightkitCanResolveSafely
    ? executableText.replace(/(^|[\s;&|()$`"'])rightkit(\s+)(cargo|rustc|rustdoc)\b/gim, '$1rightkit$2__rightkit_managed_tool__')
    : executableText;
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
  const withoutInertQuotedSpans = withoutSanctionedRightkit.replace(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g, (match, offset, full) => shellScriptFlag.test(full.slice(0, offset)) ? match : '""');
  for (const word of withoutInertQuotedSpans.matchAll(/(?:^|[\s;&|()$`"'])(\/?[^\s;&|()$`"']+)/g)) { const name = basename(word[1]); if (tools.has(name)) return name; }
  return null;
}

function denialMessage(blocked) {
  if (blocked.startsWith('github-actions-only:')) {
    return `[HOOK:rightkit-managed-build:block]\nWhy: Local public-repository execution forbidden: ${blocked.slice('github-actions-only:'.length)}\nRequired: Push changes & use managed GitHub Actions.`;
  }
  return `[HOOK:rightkit-managed-build:block]\nWhy: Direct managed build tool forbidden: ${blocked}\nRequired: Use exact rightkit entrypoint.`;
}

export function claudeHookDecision(input) {
  const command = input?.tool_input?.command ?? input?.command ?? input?.input?.command;
  const blocked = githubActionsOnlyViolation(command ?? '', {
    cwd: input?.cwd ?? input?.tool_input?.cwd ?? process.cwd(),
    environment: input?.environment ?? process.env,
  }) ?? managedBuildViolation(command ?? '');
  return blocked ? { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: denialMessage(blocked) } } : { decision: 'allow' };
}

export function clineHookDecision(input) {
  const command = input?.preToolUse?.parameters?.command ?? input?.tool_input?.command ?? input?.command;
  const blocked = githubActionsOnlyViolation(command ?? '', {
    cwd: input?.cwd ?? input?.preToolUse?.cwd ?? input?.tool_input?.cwd ?? process.cwd(),
    environment: input?.environment ?? process.env,
  }) ?? managedBuildViolation(command ?? '');
  return blocked
    ? { cancel: true, errorMessage: denialMessage(blocked) }
    : { cancel: false };
}
