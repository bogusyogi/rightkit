import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";

export const DEFAULT_APPS = Object.freeze([
  { key: "viewright", remote: "https://github.com/bogusyogi/viewright.git", appDir: "." },
  { key: "scraperight", remote: "https://github.com/bogusyogi/scraperight.git", appDir: "." },
  { key: "heardright", remote: "https://github.com/bogusyogi/heardright.git", appDir: "tauri-app-next" },
  { key: "mailright", remote: "https://github.com/bogusyogi/mailright.git", appDir: "." },
  { key: "coderight", remote: "https://github.com/bogusyogi/coderight.git", appDir: "apps/coderight-tauri" },
]);

function run(command, args, cwd) {
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    windowsHide: true,
    maxBuffer: 16 * 1024 * 1024,
    env: { ...process.env, CI: "1", GIT_TERMINAL_PROMPT: "0" },
  });
  return {
    command: [command, ...args].join(" "),
    status: result.status,
    stdout: String(result.stdout ?? "").trim().slice(-8000),
    stderr: String(result.stderr ?? "").trim().slice(-8000),
    error: result.error?.message,
  };
}

function runPnpm(args, cwd, command) {
  if (process.platform !== "win32" || command !== "pnpm") return run(command, args, cwd);
  for (const entry of String(process.env.PATH ?? "").split(path.delimiter)) {
    const bin = entry.replace(/^"|"$/g, "");
    const cli = path.join(bin, "node_modules", "pnpm", "bin", "pnpm.mjs");
    if (existsSync(cli)) return run(process.execPath, [cli, ...args], cwd);
    const shim = path.join(bin, "pnpm.cmd");
    if (existsSync(shim)) {
      if (!args.every((value) => /^[A-Za-z0-9._:@+=-]+$/.test(value))) throw new Error("unsafe pnpm argument");
      const commandLine = `"${shim}" ${args.join(" ")}`;
      const result = spawnSync(commandLine, {
        cwd,
        encoding: "utf8",
        windowsHide: true,
        maxBuffer: 16 * 1024 * 1024,
        env: { ...process.env, CI: "1", GIT_TERMINAL_PROMPT: "0" },
        shell: true,
      });
      return {
        command: commandLine,
        status: result.status,
        stdout: String(result.stdout ?? "").trim().slice(-8000),
        stderr: String(result.stderr ?? "").trim().slice(-8000),
        error: result.error?.message,
      };
    }
  }
  throw new Error("pnpm installation could not be resolved from PATH");
}

function requireSuccess(result, label) {
  if (result.status !== 0) {
    throw new Error(`${label} failed (${result.status ?? "spawn error"})\n${result.stderr || result.stdout || result.error || "no output"}`);
  }
}

function assertGeneratedWorkRoot(workRoot, tempParent) {
  const parent = path.resolve(tempParent);
  const candidate = path.resolve(workRoot);
  if (path.dirname(candidate) !== parent || !path.basename(candidate).startsWith("rightkit-standalone-")) {
    throw new Error(`refusing cleanup outside generated standalone directory: ${candidate}`);
  }
}

export async function verifyStandaloneClones({
  apps = DEFAULT_APPS,
  evidencePath = path.resolve("standalone-clone-evidence.json"),
  tempParent = tmpdir(),
  pnpmCommand = "pnpm",
} = {}) {
  mkdirSync(tempParent, { recursive: true });
  const workRoot = mkdtempSync(path.join(path.resolve(tempParent), "rightkit-standalone-"));
  const evidence = { schemaVersion: 1, generatedAt: new Date().toISOString(), workRoot, apps: [] };

  try {
    for (const app of apps) {
      const cloneRoot = path.join(workRoot, app.key);
      const clone = run("git", ["clone", "--depth", "1", "--single-branch", app.remote, cloneRoot], workRoot);
      requireSuccess(clone, `${app.key} clone`);
      const appRoot = path.resolve(cloneRoot, app.appDir);
      if (!appRoot.startsWith(`${path.resolve(cloneRoot)}${path.sep}`) && appRoot !== path.resolve(cloneRoot)) {
        throw new Error(`${app.key} package root escapes its clone`);
      }
      if (!existsSync(path.join(appRoot, "package.json"))) throw new Error(`${app.key} package root does not exist: ${app.appDir}`);
      const pkg = JSON.parse(readFileSync(path.join(appRoot, "package.json"), "utf8"));
      const expectedPnpm = String(pkg.packageManager ?? "").match(/^pnpm@(\d+\.\d+\.\d+)$/)?.[1];
      if (!expectedPnpm) throw new Error(`${app.key} must pin an exact pnpm packageManager`);
      const pnpmVersion = runPnpm(["--version"], appRoot, pnpmCommand);
      requireSuccess(pnpmVersion, `${app.key} pnpm version`);
      if (pnpmVersion.stdout !== expectedPnpm) {
        throw new Error(`${app.key} requires pnpm ${expectedPnpm}, found ${pnpmVersion.stdout}`);
      }
      const install = runPnpm(["install", "--frozen-lockfile"], appRoot, pnpmCommand);
      requireSuccess(install, `${app.key} pnpm install`);
      const doctor = runPnpm(["release:doctor"], appRoot, pnpmCommand);
      requireSuccess(doctor, `${app.key} release:doctor`);
      const revision = run("git", ["rev-parse", "HEAD"], cloneRoot);
      requireSuccess(revision, `${app.key} revision`);
      evidence.apps.push({
        key: app.key,
        remote: app.remote,
        appDir: app.appDir,
        revision: revision.stdout,
        packageManager: pkg.packageManager,
        clone,
        install,
        doctor,
      });
      mkdirSync(path.dirname(path.resolve(evidencePath)), { recursive: true });
      writeFileSync(path.resolve(evidencePath), `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    }
    return evidence;
  } finally {
    assertGeneratedWorkRoot(workRoot, tempParent);
    rmSync(workRoot, { recursive: true, force: true });
  }
}

function parseArgs(argv) {
  let evidencePath = path.resolve("standalone-clone-evidence.json");
  let selectedKeys = [];
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === "--evidence") evidencePath = path.resolve(argv[++index]);
    else if (argv[index] === "--app") selectedKeys.push(argv[++index]);
    else throw new Error(`unknown argument: ${argv[index]}`);
  }
  const apps = selectedKeys.length ? DEFAULT_APPS.filter(({ key }) => selectedKeys.includes(key)) : DEFAULT_APPS;
  if (apps.length !== (selectedKeys.length || DEFAULT_APPS.length)) throw new Error("unknown or duplicate --app value");
  return { apps, evidencePath };
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  try {
    const evidence = await verifyStandaloneClones(parseArgs(process.argv.slice(2)));
    for (const app of evidence.apps) process.stdout.write(`[standalone] ${app.key} ${app.revision.slice(0, 12)} install+doctor passed\n`);
    process.stdout.write(`[standalone] evidence ${path.resolve(parseArgs(process.argv.slice(2)).evidencePath)}\n`);
  } catch (error) {
    process.stderr.write(`[standalone] ${error.message}\n`);
    process.exitCode = 1;
  }
}
