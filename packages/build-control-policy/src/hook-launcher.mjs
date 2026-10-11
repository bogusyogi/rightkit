// Stable agent-hook launcher core. install-policy-assets.mjs copies this file byte-for-byte to <root>/launch/ and
// writes one tiny stub per hook beside it. Agents register the stub, never a content-<sha256> path, so the hook
// command text an agent hashes stays identical across policy activations. Imports node: built-ins only.
//
// Run time: read <root>/active.json -> validate the digest -> resolve <root>/content-<digest>/<fixed hook path> ->
// refuse anything that leaves the root -> import that script in this process. The target keeps reading stdin and
// writing stdout/stderr/process.exitCode itself, so the agent sees exactly what a direct `node <script>` would give.
//
// Failure rule (unchanged enforcement, not new): when the pointer or target cannot be used the launcher writes one
// line to stderr, prints nothing to stdout and exits 1, which is what `node <missing script>` already did. Claude
// Code and Codex treat that as a non-blocking hook error, and the broker/build admission gates stay authoritative.
// Real denials still come from the target (exit 2), and any error the target throws propagates untouched.
import { readFile, realpath, stat } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const LAUNCH_DIRECTORY = 'launch';
export const CORE_FILE = 'hook-launcher.mjs';
export const POINTER_FILE = 'active.json';
export const POINTER_SCHEMA = 1;
export const UNAVAILABLE_EXIT_CODE = 1;
/** Hook id -> fixed launcher name and fixed path of the hook script inside a content-<sha256> directory. */
export const HOOKS = Object.freeze({
  write: Object.freeze({ launcher: 'write-hook.mjs', target: 'src/ownership-write-hook-cli.mjs' }),
  session: Object.freeze({ launcher: 'session-hook.mjs', target: 'src/ownership-session-hook-cli.mjs' }),
});

const DIGEST = /^[0-9a-f]{64}$/;
const MAX_POINTER_BYTES = 4096;

export class LauncherRefusal extends Error {
  constructor(code, detail) {
    super(`[HOOK:rightkit-policy-launcher:unavailable] ${code}: ${detail}`);
    this.name = 'LauncherRefusal';
    this.code = code;
  }
}

const reason = (error) => error?.code ?? error?.message ?? String(error);
const contained = (parent, child) => {
  const path = relative(parent, child);
  return path !== '' && path !== '..' && !path.startsWith(`..${sep}`) && !isAbsolute(path);
};

/** The digest named by <root>/active.json, or a LauncherRefusal. The digest is the only thing used to build a path. */
export async function readActiveDigest(root) {
  const file = join(root, POINTER_FILE);
  let bytes;
  try { bytes = await readFile(file); }
  catch (error) { throw new LauncherRefusal(error?.code === 'ENOENT' ? 'pointer-missing' : 'pointer-unreadable', `${file} (${reason(error)})`); }
  if (bytes.length > MAX_POINTER_BYTES) throw new LauncherRefusal('pointer-malformed', `${file} is larger than ${MAX_POINTER_BYTES} bytes`);
  let pointer;
  try { pointer = JSON.parse(bytes.toString('utf8')); }
  catch { throw new LauncherRefusal('pointer-malformed', `${file} is not valid JSON`); }
  if (pointer === null || typeof pointer !== 'object' || Array.isArray(pointer)
    || pointer.schemaVersion !== POINTER_SCHEMA || typeof pointer.digest !== 'string') {
    throw new LauncherRefusal('pointer-malformed', `${file} must be {"schemaVersion":${POINTER_SCHEMA},"digest":"<sha256>"}`);
  }
  if (!DIGEST.test(pointer.digest)) {
    throw new LauncherRefusal('pointer-digest-invalid', `${file} digest must be 64 lowercase hex characters, got ${JSON.stringify(pointer.digest.slice(0, 80))}`);
  }
  return pointer.digest;
}

/** The real path of the active content copy of one hook script, proven to be a file inside the real root. */
export async function resolveHookTarget(hook, root) {
  if (!Object.hasOwn(HOOKS, hook)) throw new LauncherRefusal('unknown-hook', JSON.stringify(String(hook)));
  let realRoot;
  try { realRoot = await realpath(root); }
  catch (error) { throw new LauncherRefusal('root-missing', `${root} (${reason(error)})`); }
  const digest = await readActiveDigest(realRoot);
  const candidate = join(realRoot, `content-${digest}`, ...HOOKS[hook].target.split('/'));
  let target;
  try { target = await realpath(candidate); }
  catch (error) { throw new LauncherRefusal('target-missing', `${candidate} (${reason(error)})`); }
  if (!contained(realRoot, target)) throw new LauncherRefusal('target-outside-root', `${candidate} resolves to ${target}, outside ${realRoot}`);
  let info;
  try { info = await stat(target); }
  catch (error) { throw new LauncherRefusal('target-missing', `${target} (${reason(error)})`); }
  if (!info.isFile()) throw new LauncherRefusal('target-missing', `${target} is not a file`);
  return Object.freeze({ digest, target });
}

function installedRoot() {
  const here = dirname(fileURLToPath(import.meta.url));
  if (basename(here) !== LAUNCH_DIRECTORY) throw new LauncherRefusal('not-installed', `${here} is not an installed ${LAUNCH_DIRECTORY} directory`);
  return dirname(here);
}

/**
 * Run one hook through the active content copy. `deps` is a test seam: root, proc (argv/stderr/exitCode holder) and
 * importModule. Stdin and stdout are never touched here; process.argv keeps its arguments and only argv[1] becomes the
 * target script, as it would be for a direct `node <script>` run.
 */
export async function runHookLauncher(hook, { root, proc = process, importModule = (url) => import(url) } = {}) {
  try {
    const { target } = await resolveHookTarget(hook, root ?? installedRoot());
    proc.argv[1] = target;
    await importModule(pathToFileURL(target).href);
  } catch (error) {
    if (!(error instanceof LauncherRefusal)) throw error;
    proc.stderr.write(`${error.message}\n`);
    proc.exitCode = UNAVAILABLE_EXIT_CODE;
  }
}
