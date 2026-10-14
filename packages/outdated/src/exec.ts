// Bounded child-process runner. No shell, argv arrays only, a hard timeout, and
// captured output (kept in memory as a bounded tail and optionally saved to files).

import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";

export interface RunOptions {
  cwd: string;
  /** Hard timeout in milliseconds. Required: every command is bounded. */
  timeoutMs: number;
  /** Written to the child's stdin, then closed. */
  stdin?: string;
  env?: NodeJS.ProcessEnv;
  /** Save full (capped) stdout/stderr to these files. */
  stdoutPath?: string;
  stderrPath?: string;
  /** How many trailing bytes of each stream to keep in memory. Defaults to 64 KiB. */
  captureBytes?: number;
}

export interface RunResult {
  /** Exit code, or null when the process was killed by a signal or never started. */
  code: number | null;
  signal: string | null;
  timedOut: boolean;
  durationMs: number;
  /** Last bytes of stdout and stderr (bounded), as text. */
  stdout: string;
  stderr: string;
  /** Spawn failure (for example, the executable does not exist). */
  error?: string;
}

export type RunFn = (argv: string[], options: RunOptions) => Promise<RunResult>;

const TAIL_BYTES = 64 * 1024;
const FILE_CAP_BYTES = 64 * 1024 * 1024;
// Characters that cmd.exe would interpret inside a `.cmd` shim invocation.
const CMD_UNSAFE = /["%^&|<>\r\n]/;

/** Resolve an executable on Windows, where `pnpm` is a `.cmd` shim that cannot be spawned directly. */
function windowsLaunch(argv: string[], env: NodeJS.ProcessEnv): { file: string; args: string[]; verbatim: boolean } {
  const [exe, ...rest] = argv;
  if (path.extname(exe)) return finishWindows(exe, rest);
  const dirs = (env.PATH ?? env.Path ?? "").split(path.delimiter).filter(Boolean);
  for (const dir of dirs) {
    for (const ext of [".exe", ".cmd", ".bat"]) {
      const candidate = path.join(dir, exe + ext);
      if (fs.existsSync(candidate)) return finishWindows(candidate, rest);
    }
  }
  return { file: exe, args: rest, verbatim: false };
}

function finishWindows(file: string, args: string[]): { file: string; args: string[]; verbatim: boolean } {
  if (!/\.(cmd|bat)$/i.test(file)) return { file, args, verbatim: false };
  const all = [file, ...args];
  if (all.some((a) => CMD_UNSAFE.test(a))) {
    throw new Error(`argument contains characters that are unsafe for a .cmd shim: ${all.find((a) => CMD_UNSAFE.test(a))}`);
  }
  const line = all.map((a) => (/[\s]/.test(a) || a === "" ? `"${a}"` : a)).join(" ");
  return { file: process.env.ComSpec ?? "cmd.exe", args: ["/d", "/s", "/c", `"${line}"`], verbatim: true };
}

class Tail {
  private chunks: Buffer[] = [];
  private size = 0;
  constructor(private readonly limit: number) {}
  push(chunk: Buffer): void {
    this.chunks.push(chunk);
    this.size += chunk.length;
    while (this.chunks.length > 1 && this.size - this.chunks[0].length >= this.limit) {
      this.size -= this.chunks.shift()!.length;
    }
  }
  text(): string {
    const all = Buffer.concat(this.chunks);
    return all.subarray(Math.max(0, all.length - this.limit)).toString("utf8");
  }
}

function openCapture(file: string | undefined): { write: (c: Buffer) => void; end: () => Promise<void> } {
  if (!file) return { write: () => undefined, end: async () => undefined };
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const stream = fs.createWriteStream(file, { flags: "w" });
  let written = 0;
  return {
    write: (chunk) => {
      if (written >= FILE_CAP_BYTES) return;
      const slice = chunk.subarray(0, FILE_CAP_BYTES - written);
      written += slice.length;
      stream.write(slice);
    },
    end: () => new Promise((resolve) => stream.end(resolve)),
  };
}

/** Run one command to completion or timeout. Never throws. */
export const runCommand: RunFn = async (argv, options) => {
  const started = Date.now();
  const env = options.env ?? process.env;
  const stdoutTail = new Tail(options.captureBytes ?? TAIL_BYTES);
  const stderrTail = new Tail(options.captureBytes ?? TAIL_BYTES);
  const outFile = openCapture(options.stdoutPath);
  const errFile = openCapture(options.stderrPath);
  const finish = async (partial: Omit<RunResult, "durationMs" | "stdout" | "stderr">): Promise<RunResult> => {
    await Promise.all([outFile.end(), errFile.end()]);
    return { ...partial, durationMs: Date.now() - started, stdout: stdoutTail.text(), stderr: stderrTail.text() };
  };

  let launch: { file: string; args: string[]; verbatim: boolean };
  try {
    if (argv.length === 0) throw new Error("empty command");
    launch = process.platform === "win32" ? windowsLaunch(argv, env) : { file: argv[0], args: argv.slice(1), verbatim: false };
  } catch (error) {
    return finish({ code: null, signal: null, timedOut: false, error: (error as Error).message });
  }

  return new Promise<RunResult>((resolve) => {
    let timedOut = false;
    let settled = false;
    let child: ReturnType<typeof spawn>;
    try {
      child = spawn(launch.file, launch.args, {
        cwd: options.cwd,
        env,
        stdio: ["pipe", "pipe", "pipe"],
        shell: false,
        detached: process.platform !== "win32",
        windowsHide: true,
        windowsVerbatimArguments: launch.verbatim,
      });
    } catch (error) {
      void finish({ code: null, signal: null, timedOut: false, error: (error as Error).message }).then(resolve);
      return;
    }

    const killTree = () => {
      try {
        if (process.platform === "win32") {
          spawn("taskkill", ["/pid", String(child.pid), "/t", "/f"], { windowsHide: true, stdio: "ignore" });
        } else if (child.pid) {
          process.kill(-child.pid, "SIGKILL");
        }
      } catch {
        try {
          child.kill("SIGKILL");
        } catch {
          // Already gone.
        }
      }
    };
    const timer = setTimeout(() => {
      timedOut = true;
      killTree();
      // If a grandchild still holds the pipes open, do not wait for 'close' forever.
      setTimeout(() => done({ code: null, signal: "SIGKILL", timedOut: true }), 5000).unref();
    }, options.timeoutMs);

    const done = (partial: Omit<RunResult, "durationMs" | "stdout" | "stderr">) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      void finish(partial).then(resolve);
    };

    child.stdout?.on("data", (chunk: Buffer) => {
      stdoutTail.push(chunk);
      outFile.write(chunk);
    });
    child.stderr?.on("data", (chunk: Buffer) => {
      stderrTail.push(chunk);
      errFile.write(chunk);
    });
    child.on("error", (error) => done({ code: null, signal: null, timedOut, error: error.message }));
    // 'close' fires after the stdio streams end, so output is complete.
    child.on("close", (code, signal) => done({ code, signal, timedOut }));

    child.stdin?.on("error", () => undefined);
    if (options.stdin !== undefined) child.stdin?.write(options.stdin);
    child.stdin?.end();
  });
};
