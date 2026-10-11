/**
 * Windows: can Chrome's sandbox read the Chrome it is launched from?
 *
 * Chrome starts its network service and renderers inside an AppContainer. A process there reads
 * and executes only what the file's access list grants to `ALL APPLICATION PACKAGES`
 * (`S-1-15-2-1`) or, for the less privileged containers, `ALL RESTRICTED APPLICATION PACKAGES`
 * (`S-1-15-2-2`). `Program Files` carries both grants. A Chrome unpacked from an archive into a
 * user or tool folder has neither: Chrome logs "Sandbox cannot access executable ... Access is
 * denied", its network service crashes at startup, and a navigation issued meanwhile hangs or
 * fails with `net::ERR_ABORTED`.
 *
 * The installer grants the access and checks it; the launcher checks before it starts a browser.
 */
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

/** `ALL APPLICATION PACKAGES`. */
export const ALL_APPLICATION_PACKAGES = 'S-1-15-2-1';
/** `ALL RESTRICTED APPLICATION PACKAGES`. */
export const ALL_RESTRICTED_APPLICATION_PACKAGES = 'S-1-15-2-2';

const READ = 0x120089;
const EXECUTE = 0x1200a0;
const GENERIC_ALL = 0x10000000;
const GENERIC_EXECUTE = 0x20000000;
const GENERIC_READ = 0x80000000;

/** What an SDDL rights field grants: [read, execute]. */
function rights(field: string): [boolean, boolean] {
  const text = field.trim();
  if (/^0x[0-9a-f]+$/i.test(text)) {
    const mask = Number.parseInt(text.slice(2), 16);
    const has = (bits: number): boolean => ((mask & bits) >>> 0) === bits;
    const all = has(GENERIC_ALL);
    return [all || has(GENERIC_READ) || has(READ), all || has(GENERIC_EXECUTE) || has(EXECUTE)];
  }
  const pairs: string[] = text.match(/../g) ?? [];
  const all = pairs.includes('FA') || pairs.includes('GA');
  return [all || pairs.includes('FR') || pairs.includes('GR'), all || pairs.includes('FX') || pairs.includes('GX')];
}

/**
 * The AppContainer principals that the access list `sddl` does not grant read and execute.
 * Only allow entries that apply to the object itself count; an inherit-only entry (`IO`)
 * describes what children get.
 */
export function missingSandboxGrants(sddl: string): string[] {
  const granted = new Map<string, [boolean, boolean]>([[ALL_APPLICATION_PACKAGES, [false, false]], [ALL_RESTRICTED_APPLICATION_PACKAGES, [false, false]]]);
  for (const ace of sddl.split('(').slice(1)) {
    const fields = (ace.split(')')[0] ?? '').split(';');
    if (fields.length < 6 || fields[0]!.trim() !== 'A') continue;
    const flags: string[] = fields[1]!.match(/../g) ?? [];
    if (flags.includes('IO')) continue;
    const sid = fields[5]!.trim() === 'AC' ? ALL_APPLICATION_PACKAGES : fields[5]!.trim();
    const have = granted.get(sid);
    if (!have) continue;
    const [read, execute] = rights(fields[2]!);
    granted.set(sid, [have[0] || read, have[1] || execute]);
  }
  return [...granted].filter(([, [read, execute]]) => !(read && execute)).map(([sid]) => sid);
}

const icacls = (): string => path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'icacls.exe');

/** The `icacls` arguments that grant both principals read and execute on `directory` and all in it. */
export function sandboxGrantArgs(directory: string): string[] {
  return [directory, '/grant', `*${ALL_APPLICATION_PACKAGES}:(OI)(CI)(RX)`, `*${ALL_RESTRICTED_APPLICATION_PACKAGES}:(OI)(CI)(RX)`, '/T'];
}

/** The command a person runs to grant the access. */
export function sandboxGrantCommand(directory: string): string {
  const [dir, ...rest] = sandboxGrantArgs(directory);
  return `icacls "${dir}" ${rest.map((arg) => (arg.startsWith('*') ? `"${arg}"` : arg)).join(' ')}`;
}

/** The access list of `file` as SDDL text (inherited entries included), or undefined when it cannot be read. */
export function readAccessList(file: string): string | undefined {
  const dir = mkdtempSync(path.join(tmpdir(), 'rk-acl-'));
  try {
    const saved = path.join(dir, 'acl.txt');
    const done = spawnSync(icacls(), [file, '/save', saved, '/Q'], { stdio: 'ignore', windowsHide: true });
    if (done.status !== 0) return undefined;
    // icacls writes UTF-16: the file name, then its SDDL.
    return readFileSync(saved, 'utf16le').split(/\r?\n/).find((line) => line.startsWith('D:'));
  } catch {
    return undefined;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** Chrome's sandbox cannot read the browser in {@link ChromeSandboxAccessError.directory}; nothing was launched. */
export class ChromeSandboxAccessError extends Error {
  readonly code = 'CHROME_SANDBOX_ACCESS';
  constructor(
    /** The folder that needs the grant (the executable's own). */
    readonly directory: string,
    /** The principals that lack read and execute. */
    readonly principals: string[],
    /** The command that grants the access. */
    readonly fix: string = sandboxGrantCommand(directory),
  ) {
    super(`Chrome's sandbox cannot read the browser in ${directory}: no read and execute access for ${principals.join(' and ')}. Grant it with: ${fix}`);
    this.name = 'ChromeSandboxAccessError';
  }
}

export interface SandboxAccessOptions {
  platform?: NodeJS.Platform;
  /** Reads a file's access list as SDDL; injectable for tests. */
  readAccessList?: (file: string) => string | undefined;
}

/**
 * The error a launch of `executable` would run into, or undefined when the sandbox can read it,
 * on a system without AppContainers, or when the access list cannot be read (only a grant known
 * to be absent stops a launch).
 */
export function sandboxAccessProblem(executable: string, options: SandboxAccessOptions = {}): ChromeSandboxAccessError | undefined {
  if ((options.platform ?? process.platform) !== 'win32') return undefined;
  const sddl = (options.readAccessList ?? readAccessList)(executable);
  if (sddl === undefined) return undefined;
  const missing = missingSandboxGrants(sddl);
  return missing.length === 0 ? undefined : new ChromeSandboxAccessError(path.win32.dirname(executable), missing);
}

export interface GrantSandboxAccessOptions extends SandboxAccessOptions {
  /** Runs icacls; injectable for tests. */
  run?: (file: string, args: string[]) => { status: number | null };
}

/**
 * Windows only: grant Chrome's sandbox read and execute on `directory` and everything in it, then
 * check that `executable` carries the grant. Granting again is harmless. Throws when the grant
 * fails or is not there afterwards. Returns whether anything ran.
 */
export function grantSandboxAccess(directory: string, executable: string, options: GrantSandboxAccessOptions = {}): boolean {
  if ((options.platform ?? process.platform) !== 'win32') return false;
  const run = options.run ?? ((file, args) => spawnSync(file, args, { stdio: ['ignore', 'ignore', 'inherit'], windowsHide: true }));
  const done = run(icacls(), [...sandboxGrantArgs(directory), '/C', '/Q']);
  if (done.status !== 0) throw new Error(`granting Chrome's sandbox read access to ${directory} failed (icacls exit ${done.status})`);
  const sddl = (options.readAccessList ?? readAccessList)(executable);
  if (sddl === undefined) throw new Error(`the access list of ${executable} could not be read after the grant`);
  const missing = missingSandboxGrants(sddl);
  if (missing.length > 0) throw new ChromeSandboxAccessError(path.win32.dirname(executable), missing);
  return true;
}
