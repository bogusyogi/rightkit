import { existsSync, readFileSync, statSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { dirname, join, parse, resolve } from 'node:path';
import { desktopIntrusionViolation, DESKTOP_INTRUSION_REASON } from './desktop-intrusion-policy.mjs';
import { localDevelopmentPolicy, localDevelopmentCargoAllowed } from '../../build-control-core/src/local-development.mjs';

const tools = new Set(['cargo', 'rustc', 'rustdoc']);
const githubActionsOnly = 'github-actions-only';
const dynamicOperation = /(?:^|[:_-])(?:compile|test|build|package|pack|sign|qualif(?:y|ication)|smoke|release|candidate|finalize)(?:$|[:_-])/i;
const basename = (value) => value.replace(/\\/g, '/').split('/').pop().replace(/\.(?:exe|cmd)$/i, '').toLowerCase();
const trustedAgentPath = (value) => /(?:\.rightkit-managed|rightkitmanagedagent)[\\/]agent-bin/i.test(value);

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
    if (character === '#' && (index === 0 || /[\s;&|()]/.test(line[index - 1]))) break;
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

// Lex shell words without executing or expanding them. Only unquoted operators
// split commands. Substitutions are separate commands; quoted arguments stay
// intact, including shell -c scripts which are parsed only for shell executables.
function shellCommands(command) {
  const text = withoutHeredocBodies(command); const commands = [];
  let index = 0;
  function sequence(stop = null, segments = commands) {
    let words = []; let redirect = false;
    const flush = () => { if (words.length) segments.push(words); words = []; redirect = false; };
    while (index < text.length) {
      const character = text[index];
      if (character === stop) { flush(); index += 1; return; }
      if (character === '\\' && text[index + 1] === '\n') { index += 2; continue; }
      if (/[ \t\r]/.test(character)) { index += 1; continue; }
      if (character === '#') { while (index < text.length && text[index] !== '\n') index += 1; continue; }
      if (/[\n;&|()]/.test(character) && !(character === '&' && text[index + 1] === '>')) {
        flush(); index += 1;
        if (character === '(') { const nested = []; sequence(')', nested); segments.push({ commands: nested }); }
        continue;
      }
      // Redirect targets & file-descriptor numbers are arguments, never programs.
      const operator = /^(?:\d+)?(?:&>|[<>])(?:[<>&|-])?/.exec(text.slice(index));
      if (operator && !/^[<>]\(/.test(text.slice(index))) {
        index += operator[0].length; redirect = true; continue;
      }
      let word = ''; let quote = null; let started = false;
      while (index < text.length) {
        const value = text[index];
        if (!quote && (/[\s;&|()]/.test(value) || /[<>]/.test(value) && text[index + 1] !== '(')) break;
        started = true;
        if (value === '\\' && quote !== "'") {
          const next = text[index + 1];
          if (next === '\n') { index += 2; continue; }
          if (next && (quote === '"' ? /["\\$`]/ : /[\s"'\\$`;&|()<>#]/).test(next)) { word += next; index += 2; continue; }
        }
        if (value === quote) { quote = null; index += 1; continue; }
        if (!quote && (value === '"' || value === "'")) { quote = value; index += 1; continue; }
        if (quote !== "'" && ((value === '$' || !quote && /[<>]/.test(value)) && text[index + 1] === '(')) {
          index += 2; const nested = []; sequence(')', nested); segments.push({ commands: nested }); word += '__substitution__'; continue;
        }
        if (quote !== "'" && value === '`') {
          index += 1; let script = '';
          while (index < text.length && text[index] !== '`') script += text[index++];
          if (text[index] === '`') index += 1;
          segments.push({ commands: shellCommands(script) }); word += '__substitution__'; continue;
        }
        word += value; index += 1;
      }
      if (started && !redirect) words.push(word);
      redirect = false;
    }
    flush();
  }
  sequence(); return commands;
}

const assignment = /^[A-Za-z_][A-Za-z0-9_]*=/;
const shellNames = new Set(['sh', 'bash', 'zsh', 'dash', 'ksh', 'fish', 'cmd', 'powershell', 'pwsh']);
const toolName = (word = '') => tools.has(basename(word)) || /^cargo-.+/.test(basename(word)) ? basename(word) : null;
const rustupTool = (argv) => argv[1] === 'run' ? toolName(skipOptions(argv.slice(2), new Set())[1]) : null;
const packageOptions = new Set(['--dir', '-C', '--prefix', '--filter', '-F', '--workspace', '-w', '--userconfig', '--cache', '--registry', '--package', '-p']);
const launcherOptions = {
  command: [], builtin: [], exec: ['-a'], nohup: [], time: ['-o', '--output', '-f', '--format'],
  nice: ['-n', '--adjustment'], timeout: ['-k', '--kill-after'],
  stdbuf: ['-i', '-o', '-e', '--input', '--output', '--error'],
  sudo: ['-u', '-g', '-h', '-p', '-C', '-D', '-R', '--user', '--group', '--host', '--prompt', '--chdir', '--chroot'],
  xargs: ['-I', '-L', '-n', '-P', '-s', '-E', '-a', '--replace', '--max-args', '--max-lines', '--max-procs', '--max-chars', '--eof', '--arg-file', '-d', '--delimiter'],
};

function skipOptions(argv, valued) {
  let index = 0;
  while (argv[index]?.startsWith('-') && argv[index] !== '-') {
    const option = argv[index++];
    if (option === '--') break;
    if (valued.has(option)) index += 1;
  }
  return argv.slice(index);
}

// Resolve only known launchers. Never scan arbitrary arguments for tool words.
function* executableInvocations(command, inheritedPathRedirect = false) {
  yield* resolvedInvocations(shellCommands(command), inheritedPathRedirect);
}

function* resolvedInvocations(commands, inheritedPathRedirect) {
  let persistentPathRedirect = inheritedPathRedirect;
  for (const words of commands) {
    if (words.commands) { yield* resolvedInvocations(words.commands, persistentPathRedirect); continue; }
    let argv = [...words]; let pathRedirect = persistentPathRedirect;
    const assignments = () => {
      while (assignment.test(argv[0] ?? '')) {
        const value = argv.shift();
        if (/^PATH=/i.test(value)) pathRedirect = !trustedAgentPath(value.slice(5));
      }
    };
    assignments();
    if (!argv.length) { persistentPathRedirect = pathRedirect; continue; }
    if (argv[0] === 'export') {
      argv.shift(); assignments(); persistentPathRedirect = pathRedirect; continue;
    }
    // Shell control words introduce executable positions, not argument positions.
    while (['!', '{', 'if', 'then', 'elif', 'else', 'do'].includes(argv[0])) argv.shift();
    for (;;) {
      const name = basename(argv[0] ?? '');
      if (name === 'env') {
        argv = skipOptions(argv.slice(1), new Set(['-u', '--unset', '-C', '--chdir'])); assignments(); continue;
      }
      if (Object.hasOwn(launcherOptions, name)) {
        let lookup = false;
        if (name === 'command') for (const arg of argv.slice(1)) {
          if (!arg.startsWith('-') || arg === '--') break;
          if (/^-[pvV]*[vV][pvV]*$/.test(arg)) lookup = true;
        }
        if (lookup) break;
        argv = skipOptions(argv.slice(1), new Set(launcherOptions[name]));
        if (name === 'timeout') argv.shift();
        assignments(); continue;
      }
      if (name === 'rustup' && argv[1] === 'run' && !rustupTool(argv)) {
        argv = skipOptions(argv.slice(2), new Set()).slice(1); continue;
      }
      if (shellNames.has(name)) {
        let flag = -1;
        for (let index = 1; index < argv.length; index += 1) {
          const arg = argv[index];
          if (name === 'cmd' ? /^\/c$/i.test(arg) : ['powershell', 'pwsh'].includes(name) ? /^-(?:c|command)$/i.test(arg) : /^-[a-z]*c[a-z]*$/.test(arg)) { flag = index; break; }
          if (arg === '--' || !(name === 'cmd' ? arg.startsWith('/') : arg.startsWith('-'))) break;
          if (['--rcfile', '--init-file', '-o', '-O'].includes(arg)) index += 1;
        }
        if (flag >= 0 && argv[flag + 1] !== undefined) {
          yield* executableInvocations(name === 'cmd' ? argv.slice(flag + 1).join(' ') : argv[flag + 1], pathRedirect);
        }
        break;
      }
      yield { argv, pathRedirect }; break;
    }
  }
}

function packageOperation(argv) {
  const options = basename(argv[0]) === 'pnpm' ? new Set([...packageOptions].filter((option) => option !== '-w')) : packageOptions;
  const args = skipOptions(argv.slice(1), options);
  const action = args[0]?.toLowerCase();
  if (basename(argv[0]) === 'pnpm' && action === 'install' && args.length === 3
    && args.includes('--ignore-scripts') && args.some((arg) => ['--frozen-lockfile', '--lockfile-only'].includes(arg))) return null;
  if (['install', 'ci', 'add', 'update', 'upgrade'].includes(action)) return 'install';
  if (['exec', 'dlx'].includes(action) || basename(argv[0]) === 'npx') {
    const child = ['exec', 'dlx'].includes(action) ? skipOptions(args.slice(1), new Set(['--package', '-p', '-c', '--call'])) : args;
    return dynamicInvocation(child);
  }
  const script = action === 'run' || action === 'run-script' ? skipOptions(args.slice(1), packageOptions)[0] : action;
  return script && dynamicOperation.test(script) ? script : null;
}

function dynamicInvocation(argv) {
  const name = basename(argv[0] ?? '');
  if (toolName(name)) return name;
  if (name === 'rustup') return rustupTool(argv) ? 'rustup' : null;
  if (name === 'rightkit') {
    const args = argv[1] === 'remote' ? argv.slice(3) : argv.slice(1);
    return toolName(args[0]);
  }
  if (['pnpm', 'npm', 'npx', 'yarn'].includes(name)) return packageOperation(argv);
  if (name === 'right-release') {
    const operation = argv[1]?.toLowerCase();
    if (operation === 'addon' && argv[2] === 'build') return 'addon-build';
    if (operation === 'publish' && ['cargo', 'swift'].includes(argv[2])) return `publish-${argv[2]}`;
    return operation && (dynamicOperation.test(operation) || ['release', 'sign-windows', 'sign-updater', 'create-mac-updater', 'cargo-target'].includes(operation)) ? operation : null;
  }
  if (['node', 'nodejs', 'python', 'python3'].includes(name)) {
    let index = 1;
    while (argv[index]?.startsWith('-')) {
      const option = argv[index++];
      if (option === '--check') return null;
      if (option === '-m' && name.startsWith('python')) return dynamicOperation.test(argv[index] ?? '') ? argv[index] : null;
      if (['-e', '--eval', '-p', '--print', '-c'].includes(option) || /^--(?:eval|print)=|^-[epc].+/.test(option)) return null;
      if (option === '--test') return 'test';
      if (option === '--') break;
      if (['--require', '-r', '--import', '--loader'].includes(option)) index += 1;
    }
    const script = argv[index];
    if (!script || !/\.(?:mjs|cjs|js|ts|py|sh|ps1|cmd|bat)$/i.test(script)) return null;
    const scriptName = basename(script).replace(/\.[^.]+$/, '');
    if (scriptName === 'right-release') return dynamicInvocation([scriptName, ...argv.slice(index + 1)]);
    return dynamicOperation.test(scriptName) ? basename(script) : null;
  }
  // These CLIs take build/test operations as subcommands. Inspect that slot,
  // never filenames or arbitrary arguments belonging to read-only programs.
  if (['vite', 'tauri', 'next', 'webpack', 'rollup', 'playwright', 'cypress'].includes(name)
    && dynamicOperation.test(argv[1] ?? '')) return argv[1];
  if (dynamicOperation.test(name.replace(/\.(?:mjs|cjs|js|ts|sh|ps1|cmd|bat|exe)$/i, ''))) return name;
  return ['make', 'cmake', 'ninja', 'msbuild', 'xcodebuild', 'swiftc', 'tsc'].includes(name) ? name : null;
}

function dynamicCommandOperation(command) {
  for (const { argv } of executableInvocations(command)) {
    const operation = dynamicInvocation(argv);
    if (operation) return operation;
  }
  return null;
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
    const gitEntry = join(cursor, '.git');
    let orthicPublic = false;
    if (existsSync(gitEntry)) {
      try {
        let gitDir = gitEntry;
        if (!statSync(gitEntry).isDirectory()) {
          const pointer = /^gitdir:\s*(.+)$/m.exec(readFileSync(gitEntry, 'utf8'));
          if (!pointer) throw new Error('invalid gitdir');
          gitDir = resolve(cursor, pointer[1].trim());
        }
        if (existsSync(join(gitDir, 'commondir'))) gitDir = resolve(gitDir, readFileSync(join(gitDir, 'commondir'), 'utf8').trim());
        // Ask Git for exactly this repository's origin; filenames, other
        // remotes, ancestor repositories & global config cannot make it public.
        const origin = execFileSync('git', ['config', '--file', join(gitDir, 'config'), '--includes', '--get', 'remote.origin.url'], {
          cwd: cursor, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'], timeout: 2_000,
        }).trim();
        orthicPublic = /^(?:https?:\/\/github\.com\/|(?:ssh:\/\/)?git@github\.com[:/])Orthic-Labs\//i.test(origin);
      }
      catch { orthicPublic = false; }
      if (!orthicPublic) return Object.freeze({ root: null, compile: null, invalid: false, orthicPublic: false });
    }
    if (orthicPublic && existsSync(manifestPath)) {
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
  for (const { argv, pathRedirect } of executableInvocations(command)) {
    const name = basename(argv[0] ?? '');
    if (toolName(name)) return name;
    if (name === 'rustup' && rustupTool(argv)) return 'rustup-run';
    if (name === 'alias' && argv.slice(1).some((arg) => toolName(arg.slice(arg.indexOf('=') + 1).split(/\s/)[0]))) return 'alias';
    if (name === 'rightkit') {
      const remote = argv[1] === 'remote';
      const tool = toolName(argv[remote ? 3 : 1]);
      if (tool && (pathRedirect || !/^rightkit(?:\.cmd)?$/i.test(argv[0]) || remote && !['windows', 'mac', 'macos'].includes(argv[2]))) return tool;
      if (tool) continue;
    }
    // Package-manager Cargo children inherit managed shims. Only an actual
    // build invocation with a shadowed PATH is denied, never PATH text in data.
    if (pathRedirect && dynamicInvocation(argv)) return 'path-redirect';
    if (name === 'npx') {
      const child = skipOptions(argv.slice(1), new Set(['--package', '-p']));
      if (toolName(child[0])) return toolName(child[0]);
    }
  }
  return null;
}

function denialMessage(blocked) {
  if (blocked.startsWith('github-actions-only:')) {
    return `[HOOK:rightkit-managed-build:block]\nWhy: Local public-repository execution forbidden: ${blocked.slice('github-actions-only:'.length)}\nRequired: Push changes & use managed GitHub Actions.`;
  }
  return `[HOOK:rightkit-managed-build:block]\nWhy: Direct managed build tool forbidden: ${blocked}\nRequired: Use exact rightkit entrypoint.`;
}

export function claudeHookDecision(input) {
  // Codex exec_command uses `cmd`; functions.exec carries JavaScript which can
  // compose an exec_command call. Inspect its shell payload, not JavaScript
  // source or similarly shaped text passed as an ordinary shell argument.
  const command = input?.tool_input?.command ?? input?.tool_input?.cmd ?? input?.tool_input?.code ?? input?.tool_input?.input ?? input?.command ?? input?.cmd ?? input?.code ?? input?.input?.command ?? input?.input?.cmd;
  const composed = /(?:^|\.)functions[._]exec$/.test(input?.tool_name ?? '') || input?.tool_input?.code !== undefined || input?.code !== undefined;
  const composedCommands = composed && typeof command === 'string'
    ? [...command.matchAll(/\b(?:cmd|command)\s*:\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`)/g)]
      .map((match) => match[1].slice(1, -1).replace(/\\([\\'"`nrt])/g, (_, value) => ({ n: '\n', r: '\r', t: '\t' })[value] ?? value)).join('\n')
    : '';
  const inspectedCommand = composed ? composedCommands : command;
  if (desktopIntrusionViolation(inspectedCommand ?? '')) return { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: `[HOOK:desktop-intrusion:block]\nWhy: ${DESKTOP_INTRUSION_REASON}` } };
  const blocked = githubActionsOnlyViolation(inspectedCommand ?? '', {
    cwd: input?.cwd ?? input?.tool_input?.cwd ?? process.cwd(),
    environment: input?.environment ?? process.env,
  }) ?? managedBuildViolation(inspectedCommand ?? '');
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
