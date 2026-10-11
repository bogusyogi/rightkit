import { realpathSync } from 'node:fs';
import { resolveManagedRoots } from '../../release/managed-roots.mjs';
import { readFile, rename, rm, stat, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { HOOKS, LAUNCH_DIRECTORY } from './hook-launcher.mjs';
import { installPolicyAssets } from './install-policy-assets.mjs';

function quotedHookCommand(node, script) {
  const quote = (value) => `"${value.replaceAll('"', '\\"')}"`;
  return `${quote(node)} ${quote(script)}`;
}

async function readConfig(path) {
  const raw = await readFile(path, 'utf8');
  const config = JSON.parse(raw);
  return { config, mode: (await stat(path)).mode, raw, fingerprint: JSON.stringify(config) };
}

const folded = (value, platform) => {
  const text = String(value).replaceAll('\\', '/').replace(/\/{2,}/g, '/').replace(/\/+$/, '');
  return platform === 'win32' ? text.toLowerCase() : text;
};
const escapeRegExp = (value) => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/**
 * How a registered command relates to one RightKit hook: 'launcher' when it runs this root's stable launcher,
 * 'content' when it runs the hook script from a content-<sha256> directory of a managed root (the pre-launcher
 * registration that every policy activation used to invalidate), else null. The path must be the command's last
 * token and start at a token boundary, so unrelated commands that merely mention such a path are never claimed.
 */
export function registeredHookKind(command, { root, roots = [root], launcher, target, platform = process.platform }) {
  if (typeof command !== 'string') return null;
  const text = folded(command.trimEnd().replace(/["']$/, ''), platform);
  const launcherPath = `${folded(root, platform)}/${LAUNCH_DIRECTORY}/${launcher}`;
  if (text.endsWith(launcherPath) && (text.length === launcherPath.length || /["'\s]/.test(text[text.length - launcherPath.length - 1]))) return 'launcher';
  const script = escapeRegExp(folded(target, platform));
  const content = roots.some((base) => new RegExp(`(?:^|["'\\s])${escapeRegExp(folded(base, platform))}/content-[0-9a-f]{64}/${script}$`).test(text));
  return content ? 'content' : null;
}

/**
 * Register `commandText` for one hook in `groups[event]`. Every existing RightKit registration of that hook (the launcher
 * with any node path, or a content-<sha256> script of a managed root) is the same registration: the first one in a group
 * with the wanted matcher is updated in place (position and every other field kept), later or differently-matched ones
 * are dropped, and a new group is appended only when none was kept. Hooks that are not RightKit's are never touched.
 */
export function appendHook(groups, event, record, commandText, policy) {
  if (groups[event] !== undefined && !Array.isArray(groups[event])) throw new TypeError(`${event} must be an array`);
  if (!policy?.root || !policy.launcher || !policy.target) throw new TypeError('appendHook requires the policy root, launcher and target');
  const owned = (hook) => hook !== null && typeof hook === 'object' && hook.type === 'command' && registeredHookKind(hook.command, policy) !== null;
  let kept = false;
  const next = [];
  for (const group of groups[event] ?? []) {
    if (group === null || typeof group !== 'object' || !Array.isArray(group.hooks) || !group.hooks.some(owned)) { next.push(group); continue; }
    const hooks = [];
    for (const hook of group.hooks) {
      if (!owned(hook)) { hooks.push(hook); continue; }
      if (kept || group.matcher !== record.matcher) continue;
      kept = true;
      hooks.push(hook.command === commandText ? hook : { ...hook, command: commandText });
    }
    if (hooks.length > 0) next.push({ ...group, hooks });
  }
  if (!kept) next.push({ ...record, hooks: [{ type: 'command', command: commandText }] });
  groups[event] = next;
}

async function writeConfig(path, config, mode, expectedRaw) {
  const temporary = join(dirname(path), `.${randomUUID()}.tmp`);
  await writeFile(temporary, `${JSON.stringify(config, null, 2)}\n`, { mode });
  try {
    if (await readFile(path, 'utf8') !== expectedRaw) throw new Error(`Host config changed during installation: ${path}`);
    await rename(temporary, path);
  }
  catch (error) {
    await rm(temporary, { force: true });
    throw error;
  }
}

/**
 * `root` and `sourceRoot` are test seams (a temporary policy root, a copy of this package's src/). Agents are given the
 * stable launcher command, so re-running this after a policy change rewrites no host config.
 */
export async function installAgentPolicyHooks({ home = homedir(), platform = process.platform, nodePath = process.execPath, root, sourceRoot } = {}) {
  const managed = resolveManagedRoots({ platform });
  const policyRoot = root ?? managed.agentPolicy;
  const assets = await installPolicyAssets({ root: policyRoot, sourceRoot });
  const policyRoots = [...new Set([policyRoot, `${managed.legacyRoot}/agent-policy`])];
  const policy = (hook) => ({ root: policyRoot, roots: policyRoots, platform, ...HOOKS[hook] });
  const claudePath = join(home, '.claude', 'settings.json');
  const codexPath = join(home, '.codex', 'hooks.json');
  const claude = await readConfig(claudePath);
  const codex = await readConfig(codexPath);
  const writeCommand = quotedHookCommand(nodePath, assets.writeLauncher);
  const sessionCommand = quotedHookCommand(nodePath, assets.sessionLauncher);

  if (!claude.config.hooks || typeof claude.config.hooks !== 'object' || Array.isArray(claude.config.hooks)) throw new TypeError('Claude settings.json must contain a hooks object');
  appendHook(claude.config.hooks, 'PreToolUse', { matcher: 'Write|Edit|MultiEdit|Bash' }, writeCommand, policy('write'));
  appendHook(claude.config.hooks, 'SessionStart', {}, sessionCommand, policy('session'));
  if (!codex.config.hooks || typeof codex.config.hooks !== 'object' || Array.isArray(codex.config.hooks)) throw new TypeError('Codex hooks.json must contain a hooks object');
  appendHook(codex.config.hooks, 'PreToolUse', { matcher: 'Bash|apply_patch|exec_command|functions\\.exec' }, writeCommand, policy('write'));
  appendHook(codex.config.hooks, 'SessionStart', {}, sessionCommand, policy('session'));

  // A config that already holds the launcher commands is not rewritten: no reformatting, no new file, nothing for an agent to re-review.
  const claudeChanged = JSON.stringify(claude.config) !== claude.fingerprint;
  const codexChanged = JSON.stringify(codex.config) !== codex.fingerprint;
  if (claudeChanged) await writeConfig(claudePath, claude.config, claude.mode, claude.raw);
  if (codexChanged) await writeConfig(codexPath, codex.config, codex.mode, codex.raw);
  return Object.freeze({ ...assets, claudePath, codexPath, nodePath, claudeChanged, codexChanged });
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const result = await installAgentPolicyHooks();
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
