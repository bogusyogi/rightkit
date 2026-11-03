import type { Invoke } from './index.js';

/**
 * A command spec is an interface of async methods: each key is the app's command name, the one
 * parameter is the argument object (omit it for a command with no arguments), the return type is
 * the resolved value.
 *
 * ```ts
 * interface CodeRightCommands {
 *   git_status(args: { path: string }): Promise<GitStatus>;
 *   daemon_stop(): Promise<void>;
 * }
 * ```
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any -- each command has its own argument type
export type TauriCommandFn = (args: any) => Promise<unknown>;
/** Constraint for a spec interface; an interface has no index signature, so it is checked per key. */
export type TauriCommandSpec<Spec> = { [K in keyof Spec]: TauriCommandFn };

/** One entry of the runtime map. `true` calls the Rust command named like the key; a string calls that Rust name. */
export type TauriCommandEntry = true | string;

/** Runtime command map: one entry per spec key, so a command missing from it is a type error. */
export type TauriCommandMap<Spec extends TauriCommandSpec<Spec>> = {
  [K in keyof Spec & string]: TauriCommandEntry;
};

/** The error every declared command throws when `invoke` rejects, unless `mapError` is given. */
export class TauriCommandError extends Error {
  readonly command: string;
  readonly cause: unknown;
  constructor(command: string, cause: unknown) {
    super(`${command}: ${describeCause(cause)}`);
    this.name = 'TauriCommandError';
    this.command = command;
    this.cause = cause;
  }
}

export interface DefineTauriCommandsConfig<Spec extends TauriCommandSpec<Spec>> {
  invoke: Invoke;
  /** Every command of `Spec`, once. Names must be valid Rust identifiers. */
  commands: TauriCommandMap<Spec>;
  /** The one error mapper. `command` is the Rust command name that failed. Default: {@link TauriCommandError}. */
  mapError?: (error: unknown, command: string) => unknown;
}

export type TauriCommandClient<Spec extends TauriCommandSpec<Spec>> = Spec;

const RUST_IDENTIFIER = /^[A-Za-z_][A-Za-z0-9_]*$/;

/**
 * Turns a declared command map into a typed client with one error mapper. The client has exactly
 * the keys of `commands`; calling one invokes the Rust command with the argument object.
 *
 * ```ts
 * const commands = defineTauriCommands<CodeRightCommands>({
 *   invoke,
 *   commands: { git_status: true, daemon_stop: true },
 * });
 * await commands.git_status({ path });
 * ```
 */
export function defineTauriCommands<Spec extends TauriCommandSpec<Spec>>(
  config: DefineTauriCommandsConfig<Spec>,
): TauriCommandClient<Spec> {
  const { invoke, commands } = config;
  const mapError = config.mapError ?? ((error, command) => new TauriCommandError(command, error));
  const client: Record<string, (args?: Record<string, unknown>) => Promise<unknown>> = {};
  for (const [name, entry] of Object.entries(commands) as Array<[string, TauriCommandEntry]>) {
    const rustName = entry === true ? name : entry;
    if (typeof rustName !== 'string' || !RUST_IDENTIFIER.test(rustName)) {
      throw new Error(`RightKit Tauri command ${name} is not a valid command name`);
    }
    client[name] = async (args) => {
      try {
        return await invoke<unknown>(rustName, args);
      } catch (error) {
        throw mapError(error, rustName);
      }
    };
  }
  return Object.freeze(client) as unknown as TauriCommandClient<Spec>;
}

/** The Rust command names a `commands` map calls, sorted and unique. */
export function declaredTauriCommandNames<Spec extends TauriCommandSpec<Spec>>(
  commands: TauriCommandMap<Spec>,
): string[] {
  const names = new Set<string>();
  for (const [name, entry] of Object.entries(commands) as Array<[string, TauriCommandEntry]>) {
    names.add(entry === true ? name : entry);
  }
  return [...names].sort();
}

/** The command names listed in every `generate_handler![ ... ]` of a Rust source (last path segment of each item). */
export function tauriHandlerCommandNames(rustSource: string): string[] {
  const source = stripRustComments(rustSource);
  const names = new Set<string>();
  const macro = /generate_handler!\s*\[([^\]]*)\]/g;
  for (const match of source.matchAll(macro)) {
    for (const item of (match[1] ?? '').split(',')) {
      const path = item.trim().replace(/^#\[[^\]]*\]\s*/, '');
      if (path.length === 0) continue;
      const last = path.split('::').pop() ?? '';
      if (RUST_IDENTIFIER.test(last)) names.add(last);
    }
  }
  return [...names].sort();
}

/**
 * Host-side check: the declared names that no `generate_handler!` list in `rustSources` registers.
 * Run it in the app's end-to-end suite or build script with the app's Rust files; an empty array
 * means every declared command exists.
 */
export function missingTauriHandlers(
  declared: readonly string[],
  rustSources: readonly string[],
): string[] {
  const registered = new Set(rustSources.flatMap(tauriHandlerCommandNames));
  return declared.filter((name) => !registered.has(name));
}

function stripRustComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, '');
}

function describeCause(cause: unknown): string {
  if (cause instanceof Error) return cause.message;
  if (typeof cause === 'string') return cause;
  try {
    return JSON.stringify(cause) ?? String(cause);
  } catch {
    return String(cause);
  }
}
