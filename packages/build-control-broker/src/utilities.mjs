import { chmod, lstat, mkdir, readFile, unlink } from 'node:fs/promises';
import { isAbsolute, join, normalize, dirname, relative, resolve } from 'node:path';
import { parse as parseToml } from 'smol-toml';

const forbiddenBuildKeys = new Set(['target', 'target-dir', 'build-dir', 'rustc', 'rustdoc', 'rustc-wrapper', 'rustc-workspace-wrapper', 'rustflags', 'rustdocflags']);

function safeImportLibraryFlags(value) {
  if (!Array.isArray(value) || value.length === 0 || value.length % 2 !== 0) return false;
  for (let index = 0; index < value.length; index += 2) if (value[index] !== '-C' || !/^link-arg=[A-Za-z0-9_.-]+\.lib$/i.test(value[index + 1])) return false;
  return true;
}

function validateCargoConfiguration(value, path) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw Object.assign(new Error(`managed Cargo configuration must parse to a TOML table: ${path}`), { code: 'RK_CARGO_CONFIG_INVALID' });
  if (value.alias !== undefined) throw Object.assign(new Error(`managed Cargo configuration cannot define aliases: ${path}`), { code: 'RK_CARGO_CONFIG_UNSUPPORTED' });
  if (value.env !== undefined) throw Object.assign(new Error(`managed Cargo configuration cannot inject environment variables: ${path}`), { code: 'RK_CARGO_CONFIG_UNSUPPORTED' });
  for (const key of Object.keys(value.build ?? {})) if (forbiddenBuildKeys.has(key.toLowerCase())) throw Object.assign(new Error(`managed Cargo configuration cannot set build.${key}: ${path}`), { code: 'RK_CARGO_CONFIG_UNSUPPORTED' });
  for (const [target, table] of Object.entries(value.target ?? {})) {
    if (!table || typeof table !== 'object' || Array.isArray(table)) continue;
    if (table.rustdocflags !== undefined || (table.rustflags !== undefined && !safeImportLibraryFlags(table.rustflags))) throw Object.assign(new Error(`managed Cargo configuration cannot set uncontrolled target flags for ${target}: ${path}`), { code: 'RK_CARGO_CONFIG_UNSUPPORTED' });
  }
}

function ancestorDirectories(directory) {
  if (typeof directory !== 'string' || !isAbsolute(directory)) throw new TypeError('absolute directory is required');
  const values = []; let current = normalize(directory);
  for (;;) { values.push(current); const parent = dirname(current); if (parent === current) return values; current = parent; }
}

/** Parse bounded Cargo TOML structurally before allowing semantic configuration. */
export async function rejectWorkspaceCargoConfiguration(workspace, { maxBytes = 64 * 1024 } = {}) {
  for (const directory of ancestorDirectories(workspace)) for (const name of ['config', 'config.toml']) {
    const candidate = join(directory, '.cargo', name);
    const metadata = await lstat(candidate).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (!metadata) continue;
    if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > maxBytes) throw new Error(`managed Cargo configuration is not an admissible bounded file: ${candidate}`);
    let parsed;
    try { parsed = parseToml(await readFile(candidate, 'utf8')); }
    catch (error) { throw Object.assign(new Error(`managed Cargo configuration is invalid TOML: ${candidate}: ${error.message}`), { code: 'RK_CARGO_CONFIG_INVALID' }); }
    validateCargoConfiguration(parsed, candidate);
  }
}

/** Remove only a stale Unix socket; never unlink an unexpected endpoint. */
export async function removeStaleUnixSocket(endpoint, { uid = currentUid(), fs = defaultSocketFs } = {}) {
  const info = await fs.lstat(endpoint).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
  if (!info) return false;
  if (!info.isSocket()) throw new Error('refusing to remove non-socket broker endpoint');
  if (info.uid !== uid) throw new Error('refusing to remove broker socket owned by another user');
  await fs.unlink(endpoint);
  return true;
}

const defaultSocketFs = Object.freeze({ chmod, lstat, mkdir, unlink });

function currentUid() {
  if (typeof process.getuid !== 'function') throw new Error('Unix broker transport requires a current uid');
  const uid = process.getuid();
  if (!Number.isInteger(uid) || uid < 0) throw new Error('Unix broker transport requires a valid current uid');
  return uid;
}

function containedPath(root, endpoint) {
  const normalizedRoot = resolve(root);
  const normalizedEndpoint = resolve(endpoint);
  const path = relative(normalizedRoot, normalizedEndpoint);
  if (!path || path === '..' || path.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) || isAbsolute(path)) throw new Error('broker Unix socket endpoint must be contained in broker externalRoot');
  return { root: normalizedRoot, endpoint: normalizedEndpoint, parent: dirname(normalizedEndpoint) };
}

function directoryChain(root, parent) {
  const chain = [];
  for (let current = parent;; current = dirname(current)) {
    chain.push(current);
    if (current === root) return chain.reverse();
  }
}

async function inspectSocketDirectory(path, { uid, requirePrivate, fs }) {
  const info = await fs.lstat(path);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new Error(`broker socket directory must be a non-symlink directory: ${path}`);
  if (info.uid !== uid) throw new Error(`broker socket directory owner does not match current uid: ${path}`);
  const mode = info.mode & 0o777;
  if ((mode & 0o022) !== 0) throw new Error(`broker socket directory must not be group/world writable: ${path}`);
  if (requirePrivate && mode !== 0o700) throw new Error(`broker socket parent must have mode 0700: ${path}`);
}

/** Prepare a private Unix broker endpoint without relying on deployment permissions. */
export async function prepareUnixSocketEndpoint({ endpoint, externalRoot, uid = currentUid(), fs = defaultSocketFs } = {}) {
  if (typeof endpoint !== 'string' || typeof externalRoot !== 'string' || !isAbsolute(endpoint) || !isAbsolute(externalRoot)) throw new TypeError('absolute Unix socket endpoint & broker externalRoot are required');
  if (!Number.isInteger(uid) || uid < 0) throw new TypeError('valid Unix socket owner uid is required');
  const paths = containedPath(externalRoot, endpoint);
  await fs.mkdir(paths.parent, { recursive: true, mode: 0o700 });
  for (const directory of directoryChain(paths.root, paths.parent)) await inspectSocketDirectory(directory, { uid, requirePrivate: directory === paths.parent, fs });
  return paths;
}

/** Lock down a newly bound Unix socket, then prove it is still an owned socket. */
export async function secureUnixSocketEndpoint(endpoint, { uid = currentUid(), fs = defaultSocketFs } = {}) {
  await fs.chmod(endpoint, 0o600);
  const info = await fs.lstat(endpoint);
  if (!info.isSocket() || info.isSymbolicLink()) throw new Error('broker endpoint is not a Unix socket after bind');
  if (info.uid !== uid) throw new Error('broker endpoint owner does not match current uid after bind');
  if ((info.mode & 0o777) !== 0o600) throw new Error('broker endpoint mode is not 0600 after bind');
  return true;
}
