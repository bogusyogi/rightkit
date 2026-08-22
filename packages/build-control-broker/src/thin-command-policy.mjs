export class UnknownThinCommandError extends Error {
  constructor(shim, argv) {
    super(`unregistered managed command: ${shim} ${argv.join(' ')}`.trim());
    this.name = 'UnknownThinCommandError';
    this.code = 'RK_UNKNOWN_COMMAND';
  }
}

export class DeniedThinCommandError extends Error {
  constructor(shim, command, reason) {
    super(`denied managed command: ${shim} ${command} (${reason})`);
    this.name = 'DeniedThinCommandError';
    this.code = 'RK_COMMAND_DENIED';
  }
}

const capability = (lane, workspaceAccess, { compile = false, artifactWrite = false, cache = false } = {}) =>
  Object.freeze({ lane, workspaceAccess, compile, artifactWrite, cache });

const CONTROL = capability('CONTROL', 'none');
const ANALYSIS = capability('ANALYSIS', 'read');
const WORKSPACE_WRITE = capability('WORKSPACE_WRITE', 'write');
const ARTIFACT_WRITE = capability('WORKSPACE_WRITE', 'write', { artifactWrite: true });
const BUILD = capability('BUILD', 'write', { compile: true, artifactWrite: true, cache: true });
const TOOLCHAIN_WRITE = capability('TOOLCHAIN_WRITE', 'none');

const CARGO_EXTERNAL_POLICY = Object.freeze({
  about: ANALYSIS,
  outdated: ANALYSIS,
  audit: ANALYSIS,
  deny: ANALYSIS,
  machete: ANALYSIS,
  upgrade: WORKSPACE_WRITE,
  'set-version': WORKSPACE_WRITE,
  nextest: BUILD,
});

const CARGO_BUILTINS = new Set([
  'build', 'check', 'test', 'bench', 'clippy', 'doc', 'run', 'install', 'rustc', 'rustdoc',
  'clean', 'update', 'vendor', 'generate-lockfile', 'add', 'remove', 'metadata', 'tree',
  'fetch', 'fix', 'publish', 'package', 'login', 'logout', 'owner', 'yank', 'uninstall',
  'init', 'new', 'info', 'config', 'report', 'pkgid', 'verify-project', 'locate-project',
  'read-manifest', 'search', 'help', 'version'
]);

// cargo install would place unregistered executables beside digest-pinned
// managed tools. Tool provisioning is an operator path (stage, hash, register,
// promote), so agent-initiated installs fail closed (v2.8 plan §11).
const DENIED_CARGO_COMMANDS = new Map([
  ['install', 'agent tool installation is denied; managed tools are digest-registered by the operator']
]);

function commandForCargo(argv) {
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === '--') break;
    if (value.startsWith('+') && value.length > 1) continue;
    if (['--version', '-V', '-vV'].includes(value)) return value;
    if (['--manifest-path', '--config', '-C', '-Z', '--color', '--target-dir'].includes(value)) { index += 1; continue; }
    if (['--locked', '--frozen', '--offline', '--quiet', '-q', '--verbose', '-v'].includes(value)) continue;
    if (value.startsWith('--manifest-path=') || value.startsWith('--config=') || value.startsWith('--color=') || value.startsWith('-Z')) continue;
    if (value.startsWith('-')) continue;
    return value;
  }
  return null;
}

function hasFlag(argv, flag) {
  const end = argv.indexOf('--');
  return argv.slice(0, end < 0 ? argv.length : end).includes(flag);
}

function cargoCapability(command, argv) {
  if (DENIED_CARGO_COMMANDS.has(command)) throw new DeniedThinCommandError('cargo', command, DENIED_CARGO_COMMANDS.get(command));
  if (['--version', '-V', '-vV', 'version', 'help', 'locate-project', 'read-manifest'].includes(command)) return CONTROL;
  if (command === 'metadata' || command === 'tree') return hasFlag(argv, '--locked') || hasFlag(argv, '--frozen') ? ANALYSIS : WORKSPACE_WRITE;
  if (command === 'search') return ANALYSIS;
  if (command === 'fetch') return hasFlag(argv, '--locked') || hasFlag(argv, '--frozen') ? ANALYSIS : WORKSPACE_WRITE;
  if (command === 'fix') return BUILD;
  if (command === 'fmt') return hasFlag(argv, '--check') ? ANALYSIS : WORKSPACE_WRITE;
  if (['update', 'vendor', 'generate-lockfile', 'add', 'remove'].includes(command)) return WORKSPACE_WRITE;
  if (command === 'clean') return ARTIFACT_WRITE;
  if (['build', 'check', 'test', 'bench', 'clippy', 'doc', 'run', 'rustc', 'rustdoc'].includes(command)) return BUILD;
  return null;
}

function rustupCapability(argv) {
  const values = argv.filter((value) => !['--quiet', '-q', '--verbose', '-v'].includes(value));
  if (values.length === 3 && values[0] === 'target' && values[1] === 'add') return TOOLCHAIN_WRITE;
  if (['target', 'component', 'toolchain'].includes(values[0]) && ['add', 'remove', 'install', 'uninstall', 'update'].includes(values[1])) return TOOLCHAIN_WRITE;
  if (['update', 'install', 'uninstall', 'default', 'override', 'set'].includes(values[0])) return TOOLCHAIN_WRITE;
  if (values[0] === 'target' && values[1] === 'list') return CONTROL;
  if (['--version', '-V', 'show', 'which'].includes(values[0])) return CONTROL;
  if (values[0] === 'toolchain' && ['list', 'link'].includes(values[1])) return values[1] === 'list' ? CONTROL : TOOLCHAIN_WRITE;
  return null;
}

function compilerCapability(argv) {
  if (argv.length === 1 && ['--version', '-V', '-vV'].includes(argv[0])) return CONTROL;
  if (argv.some((value) => value === '--print' || value.startsWith('--print='))) return CONTROL;
  return BUILD;
}

function externalRegistrations(entries = []) {
  if (!Array.isArray(entries)) throw new TypeError('externalCommands must be an array');
  const map = new Map();
  for (const entry of entries) {
    if (!entry || entry.shim !== 'cargo' || typeof entry.command !== 'string' || !entry.command || typeof entry.toolId !== 'string' || !entry.toolId || !/^[a-f0-9]{64}$/i.test(entry.digest ?? '')) {
      throw new TypeError('invalid external command registration');
    }
    if (CARGO_BUILTINS.has(entry.command)) throw new TypeError(`external registration cannot shadow Cargo built-in: ${entry.command}`);
    if (DENIED_CARGO_COMMANDS.has(entry.command)) throw new TypeError(`external registration cannot shadow denied Cargo command: ${entry.command}`);
    if (!Object.hasOwn(CARGO_EXTERNAL_POLICY, entry.command)) throw new TypeError(`external command has no thin policy: ${entry.command}`);
    if (map.has(entry.command)) throw new TypeError(`duplicate external command: cargo:${entry.command}`);
    map.set(entry.command, Object.freeze({ ...CARGO_EXTERNAL_POLICY[entry.command], external: true, externalToolId: entry.toolId, externalDigest: entry.digest.toLowerCase() }));
  }
  return map;
}

export class ThinCommandPolicy {
  #external;
  constructor({ externalCommands = [] } = {}) { this.#external = externalRegistrations(externalCommands); }

  classify(shim, argv = []) {
    if (typeof shim !== 'string' || !shim || !Array.isArray(argv) || argv.some((value) => typeof value !== 'string')) throw new TypeError('shim and string argv are required');
    let selected = null;
    let command = null;
    if (shim === 'cargo') {
      command = commandForCargo(argv);
      selected = cargoCapability(command, argv) ?? this.#external.get(command) ?? null;
      if (command === 'machete' && selected?.external) selected = hasFlag(argv, '--fix') ? { ...selected, ...WORKSPACE_WRITE } : selected;
    } else if (shim === 'rustup') {
      command = argv[0] ?? null;
      selected = rustupCapability(argv);
    } else if (shim === 'rustc' || shim === 'rustdoc') {
      command = shim;
      selected = compilerCapability(argv);
    }
    if (!selected) throw new UnknownThinCommandError(shim, argv);
    return Object.freeze({ ...selected, shim, command, argv: Object.freeze([...argv]) });
  }

  classifyNested(parentCapability, shim, argv = []) {
    const selected = this.classify(shim, argv);
    if (!parentCapability) return selected;
    if (shim === 'cargo' && ['metadata', 'tree'].includes(selected.command)) return Object.freeze({ ...ANALYSIS, shim, command: selected.command, argv: Object.freeze([...argv]), nestedOverride: true });
    return selected;
  }
}
