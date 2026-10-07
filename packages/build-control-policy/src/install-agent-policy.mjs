import { readFile, rename, stat, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { installPolicyAssets } from './install-policy-assets.mjs';

function quotedHookCommand(node, script) {
  const quote = (value) => `"${value.replaceAll('"', '\\"')}"`;
  return `${quote(node)} ${quote(script)}`;
}

async function readConfig(path) {
  const raw = await readFile(path, 'utf8');
  const config = JSON.parse(raw);
  return { config, mode: (await stat(path)).mode, raw };
}

/** A hook command installed from an earlier agent-policy content root for the same CLI script. */
function stalePolicyHook(command, commandText) {
  const script = commandText.match(/([\w-]+-cli\.mjs)"?$/)?.[1];
  return Boolean(script) && command !== commandText && /agent-policy[\\/]+content-[0-9a-f]{64}/.test(command) && command.replace(/"$/, '').endsWith(script);
}

export function appendHook(groups, event, record, commandText) {
  if (groups[event] !== undefined && !Array.isArray(groups[event])) throw new TypeError(`${event} must be an array`);
  // Policy assets are content-addressed, so every update moves the hook path: drop the superseded entry instead of running both.
  const current = (groups[event] ?? [])
    .map((group) => ({ ...group, hooks: (group.hooks ?? []).filter((hook) => !(hook.type === 'command' && stalePolicyHook(hook.command, commandText))) }))
    .filter((group) => group.hooks.length > 0);
  const duplicate = current.some((group) => group.matcher === record.matcher
    && (group.hooks ?? []).some((hook) => hook.type === 'command' && hook.command === commandText));
  if (!duplicate) current.push({ ...record, hooks: [{ type: 'command', command: commandText }] });
  groups[event] = current;
}

async function writeConfig(path, config, mode, expectedRaw) {
  const temporary = join(dirname(path), `.${randomUUID()}.tmp`);
  await writeFile(temporary, `${JSON.stringify(config, null, 2)}\n`, { mode });
  try {
    if (await readFile(path, 'utf8') !== expectedRaw) throw new Error(`Host config changed during installation: ${path}`);
    await rename(temporary, path);
  }
  catch (error) {
    const { rm } = await import('node:fs/promises');
    await rm(temporary, { force: true });
    throw error;
  }
}

export async function installAgentPolicyHooks({ home = homedir(), platform = process.platform, nodePath = process.execPath } = {}) {
  const assets = await installPolicyAssets({ root: platform === 'win32' ? 'D:\\.rightkit-managed\\agent-policy' : '/Volumes/D/.rightkit-managed/agent-policy' });
  const claudePath = join(home, '.claude', 'settings.json');
  const codexPath = join(home, '.codex', 'hooks.json');
  const claude = await readConfig(claudePath);
  const codex = await readConfig(codexPath);
  const writeCommand = quotedHookCommand(nodePath, assets.writeHook);
  const sessionCommand = quotedHookCommand(nodePath, assets.sessionHook);

  if (!claude.config.hooks || typeof claude.config.hooks !== 'object' || Array.isArray(claude.config.hooks)) throw new TypeError('Claude settings.json must contain a hooks object');
  appendHook(claude.config.hooks, 'PreToolUse', { matcher: 'Write|Edit|MultiEdit' }, writeCommand);
  appendHook(claude.config.hooks, 'SessionStart', {}, sessionCommand);
  if (!codex.config.hooks || typeof codex.config.hooks !== 'object' || Array.isArray(codex.config.hooks)) throw new TypeError('Codex hooks.json must contain a hooks object');
  appendHook(codex.config.hooks, 'PreToolUse', { matcher: 'Bash|apply_patch|exec_command|functions\\.exec' }, writeCommand);
  appendHook(codex.config.hooks, 'SessionStart', {}, sessionCommand);

  await writeConfig(claudePath, claude.config, claude.mode, claude.raw);
  await writeConfig(codexPath, codex.config, codex.mode, codex.raw);
  return Object.freeze({ ...assets, claudePath, codexPath, nodePath });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = await installAgentPolicyHooks();
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
