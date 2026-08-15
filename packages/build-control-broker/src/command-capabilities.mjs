export class UnknownManagedCommandError extends Error {
  constructor(shim, argv) {
    super(`unregistered managed command: ${shim} ${argv.join(' ')}`.trim());
    this.name = 'UnknownManagedCommandError'; this.code = 'RK_UNKNOWN_COMMAND';
  }
}

const control = (command) => Object.freeze({ command, lane: 'CONTROL', buildCapable: false });
const build = (command) => Object.freeze({ command, lane: 'BUILD', buildCapable: true });
const workspaceWrite = (command) => Object.freeze({ command, lane: 'WORKSPACE_WRITE', buildCapable: false });
const maintenance = (operation) => Object.freeze({ operation, lane: 'MAINTENANCE', buildCapable: false, internal: true });
const external = (command, lane, buildCapable) => Object.freeze({ command, lane, buildCapable, external: true });

/** Explicitly registered managed tool capabilities. Unknown commands reject. */
export const DEFAULT_COMMAND_CAPABILITIES = Object.freeze({
  cargo: Object.freeze({
    '--version': control('--version'), '-V': control('-V'), '-vV': control('-vV'), version: control('version'), help: control('help'), metadata: workspaceWrite('metadata'), 'locate-project': control('locate-project'), 'read-manifest': control('read-manifest'), search: control('search'), about: external('about', 'CONTROL', false), fmt: workspaceWrite('fmt'), tree: control('tree'),
    clean: workspaceWrite('clean'), update: workspaceWrite('update'), vendor: workspaceWrite('vendor'), 'generate-lockfile': workspaceWrite('generate-lockfile'),
    build: build('build'), check: build('check'), test: build('test'), bench: build('bench'), clippy: build('clippy'), doc: build('doc'), install: build('install'), nextest: external('nextest', 'BUILD', true), run: build('run')
  }),
  rustup: Object.freeze({
    '--version': control('--version'), '-V': control('-V'), show: control('show')
  }),
  rustc: Object.freeze({ '--version': control('--version'), '-V': control('-V'), '-vV': control('-vV'), '*': build('rustc') }),
  rustdoc: Object.freeze({ '--version': control('--version'), '-V': control('-V'), '-vV': control('-vV'), '*': build('rustdoc') })
});

/** Explicit broker-only maintenance operations; no Cargo command maps here. */
export const DEFAULT_INTERNAL_CAPABILITIES = Object.freeze({
  gc: maintenance('gc'), inventory: maintenance('inventory'), 'orphan-cleanup': maintenance('orphan-cleanup')
});

export class CommandCapabilityRegistry {
  #capabilities;
  constructor(capabilities = DEFAULT_COMMAND_CAPABILITIES) {
    if (!capabilities || typeof capabilities !== 'object') throw new TypeError('capabilities must be an object');
    this.#capabilities = capabilities;
  }
  classify(shim, argv = []) {
    if (typeof shim !== 'string' || !shim || !Array.isArray(argv) || !argv.every((value) => typeof value === 'string')) throw new TypeError('shim & string argv are required');
    const command = commandFor(shim, argv);
    let capability = this.#capabilities[shim]?.[command];
    if (!capability && this.#capabilities[shim]?.['*']) {
      capability = shim === 'rustc' || shim === 'rustdoc' || isDigestPinned(this.#capabilities[shim]['*'])
        ? this.#capabilities[shim]['*'] : null;
    }
    if (!capability || (capability.external && !isExternalRegistration(capability))) throw new UnknownManagedCommandError(shim, argv);
    const resolved = resolveCargoCapability(shim, command, argv, capability);
    return Object.freeze({ ...resolved, shim, argv: Object.freeze([...argv]) });
  }
}

/** Build a registry with explicitly digest-pinned external Cargo tools. */
export function createCommandCapabilityRegistry({ externalCommands = [] } = {}) {
  if (!Array.isArray(externalCommands)) throw new TypeError('externalCommands must be an array');
  const cargo = { ...DEFAULT_COMMAND_CAPABILITIES.cargo };
  const seen = new Set();
  for (const entry of externalCommands) {
    const key = `${entry?.shim}:${entry?.command}`;
    if (seen.has(key)) throw new TypeError(`duplicate external command: ${key}`);
    seen.add(key);
    if (entry?.shim !== 'cargo' || typeof entry.command !== 'string' || !entry.command || !['CONTROL', 'BUILD', 'WORKSPACE_WRITE'].includes(entry.lane) || typeof entry.toolId !== 'string' || !entry.toolId || !/^[a-f0-9]{64}$/i.test(entry.digest)) throw new TypeError('invalid external command registration');
    if (entry.command !== 'about' && entry.command !== 'nextest') throw new TypeError(`unsupported external Cargo command: ${entry.command}`);
    cargo[entry.command] = Object.freeze({ command: entry.command, lane: entry.lane, buildCapable: entry.lane === 'BUILD', external: true, externalToolId: entry.toolId, externalDigest: entry.digest, digest: entry.digest });
  }
  return new CommandCapabilityRegistry({ ...DEFAULT_COMMAND_CAPABILITIES, cargo: Object.freeze(cargo) });
}

export const commandCapabilities = new CommandCapabilityRegistry();
export const classifyManagedCommand = (shim, argv = []) => commandCapabilities.classify(shim, argv);
export const classifyBrokerOperation = (operation) => {
  if (typeof operation !== 'string' || !operation) throw new TypeError('broker operation is required');
  const capability = DEFAULT_INTERNAL_CAPABILITIES[operation];
  if (!capability) throw new UnknownManagedCommandError('broker', [operation]);
  return capability;
};

function commandFor(shim, argv) {
  if (shim === 'rustc' || shim === 'rustdoc') return argv.length === 1 && ['--version', '-V', '-vV'].includes(argv[0]) ? argv[0] : '*';
  if (shim === 'rustup') return rustupCommand(argv);
  if (shim !== 'cargo') return null;
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === '--') break;
    if (value.startsWith('+') && value.length > 1) continue;
    if (['--version', '-V', '-vV'].includes(value)) return value;
    if (value === '--manifest-path' || value === '--config' || value === '-C' || value === '-Z') { index += 1; continue; }
    if (value.startsWith('--manifest-path=') || value.startsWith('--config=') || value.startsWith('-Z')) continue;
    if (value.startsWith('-')) continue;
    return value;
  }
  return null;
}

function rustupCommand(argv) {
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === '--') break;
    if (['--version', '-V'].includes(value)) return value;
    if (value === '--profile') { index += 1; continue; }
    if (value.startsWith('--profile=')) continue;
    if (value.startsWith('-')) continue;
    return value;
  }
  return null;
}

function isDigestPinned(capability) {
  return typeof capability?.digest === 'string' && /^[a-f0-9]{64}$/i.test(capability.digest);
}

function isExternalRegistration(capability) {
  return typeof capability?.externalToolId === 'string' && capability.externalToolId.length > 0 && isDigestPinned(capability);
}

function resolveCargoCapability(shim, command, argv, capability) {
  if (shim !== 'cargo') return capability;
  if (command === 'fmt') return hasFlagAfterCommand(argv, command, '--check') ? control('fmt') : workspaceWrite('fmt');
  if (command === 'metadata') {
    // Cargo may update Cargo.lock while resolving metadata; lock flags prove read-only behavior.
    return hasFlagBeforeTerminator(argv, '--locked') || hasFlagBeforeTerminator(argv, '--frozen') ? control('metadata') : workspaceWrite('metadata');
  }
  return capability;
}

function hasFlagAfterCommand(argv, command, flag) {
  const commandIndex = argv.indexOf(command);
  if (commandIndex < 0) return false;
  const terminator = argv.indexOf('--', commandIndex + 1);
  return argv.slice(commandIndex + 1, terminator < 0 ? argv.length : terminator).includes(flag);
}

function hasFlagBeforeTerminator(argv, flag) {
  const terminator = argv.indexOf('--');
  return argv.slice(0, terminator < 0 ? argv.length : terminator).includes(flag);
}
