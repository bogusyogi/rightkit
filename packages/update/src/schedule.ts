// `right-update schedule install|remove|status`: a user-level recurring run.
//
// macOS: a launchd user agent. Windows: a Task Scheduler task that runs PowerShell hidden.
// The scheduled command is `right-update run --repo <repo> --quiet`, which performs
// check, apply, gate and land in one process and logs JSON lifecycle events.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { runCommand, type RunFn } from "./exec.js";
import { repoSlug, writeFileAtomic } from "./state.js";

export const DEFAULT_INTERVAL_MINUTES = 1440;
export const MIN_INTERVAL_MINUTES = 15;
export const MAX_INTERVAL_MINUTES = 10_080;

export interface ScheduleSpec {
  repo: string;
  intervalMinutes: number;
  /** Absolute path of the node executable. */
  nodePath: string;
  /** Absolute path of the right-update bin script. */
  binPath: string;
  /** PATH to bake into the job, so node, git, pnpm and rightkit resolve under launchd. */
  pathEnv: string;
  home: string;
  platform: NodeJS.Platform;
  /** Windows only: `%LOCALAPPDATA%`. */
  localAppData?: string;
  /** Windows only: DOMAIN\user. */
  userName?: string;
}

export function validateInterval(minutes: number): string | null {
  if (!Number.isInteger(minutes) || minutes < MIN_INTERVAL_MINUTES || minutes > MAX_INTERVAL_MINUTES) {
    return `--interval must be an integer from ${MIN_INTERVAL_MINUTES} to ${MAX_INTERVAL_MINUTES} minutes`;
  }
  return null;
}

export function launchdLabel(repo: string): string {
  return `com.rightkit.update.${repoSlug(repo).slug}`;
}

export function launchdPlistPath(spec: Pick<ScheduleSpec, "repo" | "home">): string {
  return path.join(spec.home, "Library", "LaunchAgents", `${launchdLabel(spec.repo)}.plist`);
}

export function macLogPath(spec: Pick<ScheduleSpec, "repo" | "home">): string {
  return path.join(spec.home, "Library", "Logs", "RightKit", "update", `${repoSlug(spec.repo).slug}.log`);
}

export function windowsLogPath(spec: Pick<ScheduleSpec, "repo" | "home" | "localAppData">): string {
  const base = spec.localAppData ?? path.win32.join(spec.home, "AppData", "Local");
  return path.win32.join(base, "RightKit", "Logs", "update", `${repoSlug(spec.repo).slug}.log`);
}

export function windowsTaskName(repo: string): string {
  return `\\RightKit\\update-${repoSlug(repo).slug}`;
}

function xml(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** The launchd property list for a repo. Pure: no file access. */
export function renderLaunchdPlist(spec: ScheduleSpec): string {
  const log = macLogPath(spec);
  const args = [spec.nodePath, spec.binPath, "run", "--repo", spec.repo, "--quiet"];
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${xml(launchdLabel(spec.repo))}</string>
  <key>ProgramArguments</key>
  <array>
${args.map((a) => `    <string>${xml(a)}</string>`).join("\n")}
  </array>
  <key>WorkingDirectory</key>
  <string>${xml(spec.repo)}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>${xml(spec.pathEnv)}</string>
  </dict>
  <key>StartInterval</key>
  <integer>${spec.intervalMinutes * 60}</integer>
  <key>RunAtLoad</key>
  <false/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityIO</key>
  <true/>
  <key>StandardOutPath</key>
  <string>${xml(log)}</string>
  <key>StandardErrorPath</key>
  <string>${xml(log)}</string>
</dict>
</plist>
`;
}

/** Windows-style paths are built with win32 joins even in tests on other hosts; map them back for real file access. */
function hostFsPath(p: string): string {
  return process.platform === "win32" ? p : p.replace(/\\/g, "/");
}

function psQuote(text: string): string {
  if (/["%\r\n]/.test(text)) throw new Error(`path contains characters that cannot be scheduled safely: ${text}`);
  return `'${text.replace(/'/g, "''")}'`;
}

/** ISO 8601 duration for a whole number of minutes, for example 90 -> PT1H30M and 1500 -> P1DT1H. */
export function isoDuration(minutes: number): string {
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  const mins = minutes % 60;
  const time = `${hours ? `${hours}H` : ""}${mins ? `${mins}M` : ""}`;
  return `P${days ? `${days}D` : ""}${time ? `T${time}` : ""}`;
}

/** The PowerShell argument string: hidden, non-interactive, appending to the log. */
export function windowsTaskArguments(spec: ScheduleSpec): string {
  const log = windowsLogPath(spec);
  const inner = `& ${psQuote(spec.nodePath)} ${psQuote(spec.binPath)} run --repo ${psQuote(spec.repo)} --quiet 2>&1 | Out-File -Append -Encoding utf8 -LiteralPath ${psQuote(log)}`;
  return `-WindowStyle Hidden -NonInteractive -NoProfile -Command "${inner}"`;
}

/**
 * Task Scheduler definition. It is registered from XML because `schtasks /Create /TR` caps the
 * command line at 261 characters, which real install paths exceed. The task runs as the current
 * user with a limited token, only while logged on, and is hidden.
 */
export function renderTaskXml(spec: ScheduleSpec, start: Date = new Date()): string {
  const boundary = start.toISOString().replace(/\.\d+Z$/, "");
  return `<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>${xml(`right-update scheduled run for ${spec.repo}`)}</Description>
  </RegistrationInfo>
  <Triggers>
    <TimeTrigger>
      <Repetition>
        <Interval>${isoDuration(spec.intervalMinutes)}</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>${boundary}</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <AllowHardTerminate>true</AllowHardTerminate>
    <ExecutionTimeLimit>PT4H</ExecutionTimeLimit>
    <Hidden>true</Hidden>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>powershell.exe</Command>
      <Arguments>${xml(windowsTaskArguments(spec))}</Arguments>
      <WorkingDirectory>${xml(spec.repo)}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
`;
}

export function windowsTaskXmlPath(spec: Pick<ScheduleSpec, "repo" | "home" | "localAppData">): string {
  const base = spec.localAppData ?? path.win32.join(spec.home, "AppData", "Local");
  return path.win32.join(base, "RightKit", "update", `${repoSlug(spec.repo).slug}.task.xml`);
}

/** `schtasks /Create` arguments for a task defined by an XML file. */
export function windowsCreateArgs(spec: ScheduleSpec, xmlPath: string = windowsTaskXmlPath(spec)): string[] {
  return ["schtasks", "/Create", "/TN", windowsTaskName(spec.repo), "/XML", xmlPath, "/F"];
}

export interface ScheduleResult {
  exitCode: 0 | 1 | 2;
  action: "install" | "remove" | "status";
  platform: string;
  label: string;
  installed: boolean;
  loaded?: boolean;
  intervalMinutes?: number;
  files: string[];
  log?: string;
  message: string;
}

export interface ScheduleDeps {
  run?: RunFn;
  platform?: NodeJS.Platform;
  home?: string;
  env?: NodeJS.ProcessEnv;
  nodePath?: string;
  binPath?: string;
  uid?: number;
}

function realBin(file: string | undefined): string {
  if (!file) return "";
  try {
    return fs.realpathSync(file);
  } catch {
    return file;
  }
}

function specFor(repo: string, intervalMinutes: number, deps: ScheduleDeps): ScheduleSpec {
  const env = deps.env ?? process.env;
  return {
    repo,
    intervalMinutes,
    nodePath: deps.nodePath ?? process.execPath,
    binPath: deps.binPath ?? realBin(process.argv[1]),
    pathEnv: env.PATH ?? "/usr/bin:/bin:/usr/sbin:/sbin",
    home: deps.home ?? os.homedir(),
    platform: deps.platform ?? process.platform,
    localAppData: env.LOCALAPPDATA,
    userName: env.USERNAME ? (env.USERDOMAIN ? `${env.USERDOMAIN}\\${env.USERNAME}` : env.USERNAME) : undefined,
  };
}

export async function runSchedule(
  action: "install" | "remove" | "status",
  options: { repo: string; intervalMinutes?: number },
  deps: ScheduleDeps = {},
): Promise<ScheduleResult> {
  const repo = path.resolve(options.repo);
  const run = deps.run ?? runCommand;
  const interval = options.intervalMinutes ?? DEFAULT_INTERVAL_MINUTES;
  const platform = deps.platform ?? process.platform;
  const base = { action, platform: String(platform), files: [] as string[] };

  if (action === "install") {
    const problem = validateInterval(interval);
    if (problem) return { ...base, exitCode: 2, label: "", installed: false, message: problem };
    if (!fs.existsSync(path.join(repo, ".rightkit", "update.json"))) {
      return { ...base, exitCode: 2, label: "", installed: false, message: `${repo} has no .rightkit/update.json` };
    }
  }

  if (platform === "darwin") {
    const spec = specFor(repo, interval, deps);
    const label = launchdLabel(repo);
    const plist = launchdPlistPath(spec);
    const uid = deps.uid ?? process.getuid?.() ?? 0;
    const domain = `gui/${uid}`;
    const log = macLogPath(spec);
    if (action === "install") {
      if (!path.isAbsolute(spec.binPath) || !fs.existsSync(spec.binPath)) {
        return { ...base, exitCode: 2, label, installed: false, message: `right-update bin script not found: ${spec.binPath}` };
      }
      fs.mkdirSync(path.dirname(log), { recursive: true });
      writeFileAtomic(plist, renderLaunchdPlist(spec));
      await run(["launchctl", "bootout", `${domain}/${label}`], { cwd: spec.home, timeoutMs: 30_000 });
      const boot = await run(["launchctl", "bootstrap", domain, plist], { cwd: spec.home, timeoutMs: 30_000 });
      if (boot.code !== 0) {
        return { ...base, exitCode: 2, label, installed: true, loaded: false, files: [plist], log, message: `wrote ${plist}, but launchctl bootstrap failed: ${boot.stderr.trim() || boot.error}` };
      }
      return { ...base, exitCode: 0, label, installed: true, loaded: true, intervalMinutes: interval, files: [plist], log, message: `installed ${label} (every ${interval} min); log ${log}` };
    }
    if (action === "remove") {
      await run(["launchctl", "bootout", `${domain}/${label}`], { cwd: spec.home, timeoutMs: 30_000 });
      const existed = fs.existsSync(plist);
      fs.rmSync(plist, { force: true });
      return { ...base, exitCode: 0, label, installed: false, loaded: false, files: existed ? [plist] : [], log, message: existed ? `removed ${label}` : `${label} was not installed` };
    }
    const print = await run(["launchctl", "print", `${domain}/${label}`], { cwd: spec.home, timeoutMs: 30_000 });
    const installed = fs.existsSync(plist);
    return {
      ...base,
      exitCode: installed ? 0 : 1,
      label,
      installed,
      loaded: print.code === 0,
      files: installed ? [plist] : [],
      log,
      message: installed ? `${label}: ${print.code === 0 ? "loaded" : "plist present but not loaded"}` : `${label} is not installed`,
    };
  }

  if (platform === "win32") {
    const spec = specFor(repo, interval, deps);
    const name = windowsTaskName(repo);
    const log = windowsLogPath(spec);
    const cwd = spec.home;
    if (action === "install") {
      let args: string[];
      let xmlText: string;
      const xmlPath = windowsTaskXmlPath(spec);
      try {
        xmlText = renderTaskXml(spec);
        args = windowsCreateArgs(spec, xmlPath);
      } catch (error) {
        return { ...base, exitCode: 2, label: name, installed: false, message: (error as Error).message };
      }
      fs.mkdirSync(hostFsPath(path.win32.dirname(log)), { recursive: true });
      fs.mkdirSync(hostFsPath(path.win32.dirname(xmlPath)), { recursive: true });
      // schtasks /XML expects UTF-16 with a byte order mark.
      fs.writeFileSync(hostFsPath(xmlPath), Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(xmlText, "utf16le")]));
      const created = await run(args, { cwd, timeoutMs: 60_000 });
      if (created.code !== 0) return { ...base, exitCode: 2, label: name, installed: false, message: `schtasks /Create failed: ${created.stderr.trim() || created.stdout.trim() || created.error}` };
      return { ...base, exitCode: 0, label: name, installed: true, loaded: true, intervalMinutes: interval, files: [xmlPath], log, message: `installed task ${name} (every ${interval} min); log ${log}` };
    }
    if (action === "remove") {
      const deleted = await run(["schtasks", "/Delete", "/TN", name, "/F"], { cwd, timeoutMs: 60_000 });
      return { ...base, exitCode: deleted.code === 0 ? 0 : 1, label: name, installed: false, files: [], log, message: deleted.code === 0 ? `removed task ${name}` : `task ${name} was not installed` };
    }
    const query = await run(["schtasks", "/Query", "/TN", name, "/FO", "LIST", "/V"], { cwd, timeoutMs: 60_000 });
    const installed = query.code === 0;
    return { ...base, exitCode: installed ? 0 : 1, label: name, installed, loaded: installed, files: [], log, message: installed ? query.stdout.trim() : `task ${name} is not installed` };
  }

  return { ...base, exitCode: 2, label: "", installed: false, message: `schedule is supported on macOS and Windows only, not ${platform}` };
}
