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

const commandCapability = (lane, workspaceAccess, { compile = false, artifactWrite = false, cache = false, residentAfterBuild = false } = {}) =>
  Object.freeze({ lane, workspaceAccess, compile, artifactWrite, cache, residentAfterBuild });

const CONTROL = commandCapability('CONTROL', 'none');
const ANALYSIS = commandCapability('ANALYSIS', 'read');
const WORKSPACE_WRITE = commandCapability('WORKSPACE_WRITE', 'write');
const ARTIFACT_WRITE = commandCapability('WORKSPACE_WRITE', 'write', { artifactWrite: true });
const BUILD = commandCapability('BUILD', 'write', { compile: true, artifactWrite: true, cache: true });
// `cargo run` is a writer only until Cargo launches its executable. Once Cargo
// reports `Running`, no compiler remains in that Cargo invocation; keeping the
// BUILD permit for application lifetime would serialize unrelated development.
const CARGO_RUN = commandCapability('BUILD', 'write', { compile: true, artifactWrite: true, cache: true, residentAfterBuild: true });
const TOOLCHAIN_WRITE = commandCapability('TOOLCHAIN_WRITE', 'none');
// `cargo login` writes the registry token into the broker-managed CARGO_HOME, where managed
// `cargo publish` reads it. The token arrives only as the request's credentialStdin and is piped to
// cargo's stdin, never as argv: argv is logged, listed, and visible in process tables.
const CREDENTIAL_LOGIN = Object.freeze({ ...commandCapability('CONTROL', 'none'), credentialStdin: true });

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
  ['install', 'agent tool installation is denied; managed tools are digest-registered by the operator'],
  ['uninstall', 'agent tool removal is denied; managed tools are digest-registered by the operator']
]);

/** Command-class cost bucket for compile-capable roots (dual-host routing §5).
 * One declarative classification next to the capability table: analysis
 * (check/clippy), test (test/bench/nextest), everything else that compiles is a
 * build-class root. Non-compiling capabilities return null. */
export function compileClass(capability) {
  if (!capability || typeof capability !== 'object' || capability.compile !== true) return null;
  if (capability.command === 'check' || capability.command === 'clippy') return 'ANALYSIS';
  if (capability.command === 'test' || capability.command === 'bench' || capability.command === 'nextest') return 'TEST';
  return 'BUILD';
}

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

const NO_ARGV_TOKEN = 'pass registry tokens only through `rightkit cargo login` (prompted or piped on stdin), never on the command line';
const LOGIN_FLAGS_WITH_VALUE = new Set(['--registry', '--color', '--manifest-path', '--config', '-C', '-Z']);
const LOGIN_FLAGS = new Set(['--quiet', '-q', '--verbose', '-v', '-vv', '--locked', '--frozen', '--offline']);

/** `cargo login` may take only flags: a positional token or `--` credential-provider arguments would leak the secret into argv. */
function assertLoginArgv(argv) {
  let seenCommand = false;
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (!seenCommand) { if (value === 'login') seenCommand = true; else if (LOGIN_FLAGS_WITH_VALUE.has(value)) index += 1; continue; }
    if (LOGIN_FLAGS_WITH_VALUE.has(value)) { index += 1; continue; }
    if (LOGIN_FLAGS.has(value) || /^--(registry|color|config)=/.test(value)) continue;
    throw new DeniedThinCommandError('cargo', 'login', NO_ARGV_TOKEN);
  }
}

function assertNoArgvToken(command, argv) {
  const end = argv.indexOf('--');
  if (argv.slice(0, end < 0 ? argv.length : end).some((value) => value === '--token' || value.startsWith('--token='))) throw new DeniedThinCommandError('cargo', command, NO_ARGV_TOKEN);
}

function cargoCapability(command, argv) {
  if (DENIED_CARGO_COMMANDS.has(command)) throw new DeniedThinCommandError('cargo', command, DENIED_CARGO_COMMANDS.get(command));
  if (['--version', '-V', '-vV', 'version', 'help', 'locate-project', 'read-manifest'].includes(command)) return CONTROL;
  if (command === 'metadata' || command === 'tree') return hasFlag(argv, '--locked') || hasFlag(argv, '--frozen') ? ANALYSIS : WORKSPACE_WRITE;
  if (command === 'search') return ANALYSIS;
  // Read-only queries: registry/manifest/config inspection, never a build or workspace write.
  if (['info', 'pkgid', 'verify-project', 'report', 'config'].includes(command)) return ANALYSIS;
  // Scaffolding writes new files into the workspace.
  if (['new', 'init'].includes(command)) return WORKSPACE_WRITE;
  if (command === 'fetch') return hasFlag(argv, '--locked') || hasFlag(argv, '--frozen') ? ANALYSIS : WORKSPACE_WRITE;
  if (command === 'fix') return BUILD;
  if (command === 'fmt') return hasFlag(argv, '--check') ? ANALYSIS : WORKSPACE_WRITE;
  if (['update', 'vendor', 'generate-lockfile', 'add', 'remove'].includes(command)) return WORKSPACE_WRITE;
  if (command === 'clean') return ARTIFACT_WRITE;
  if (command === 'run') return CARGO_RUN;
  if (command === 'login') { assertLoginArgv(argv); return CREDENTIAL_LOGIN; }
  // Registry account operations use the broker-managed credentials from `rightkit cargo login`.
  if (command === 'logout') return CONTROL;
  if (['owner', 'yank'].includes(command)) { assertNoArgvToken(command, argv); return CONTROL; }
  // `cargo package` verifies by compiling the packaged crate into target/package.
  if (command === 'package') return BUILD;
  if (command === 'publish') { assertNoArgvToken(command, argv); return BUILD; }
  if (['build', 'check', 'test', 'bench', 'clippy', 'doc', 'rustc', 'rustdoc'].includes(command)) return BUILD;
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
