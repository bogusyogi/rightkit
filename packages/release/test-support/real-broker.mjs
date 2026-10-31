// Test support: start the INSTALLED RustRight broker as an isolated instance, and run the installed `rustright` client against it.
//
// There is no simulation here. The broker is the installed `rustright-broker` binary, the client is the installed `rustright` binary, and every answer a test
// reads is theirs. What is isolated is the state they keep, never their behaviour:
//   - the broker's config is the host's own installed `config.json` (tools and digests, adapters, admission, bounds, limits) with every writable location moved
//     under one temp root: control root, data root, state file, receipts, activation fence, temp root, socket (or a unique named pipe on Windows), and the
//     sccache gateway (disabled, its port a free one, never the live 4227). The same recipe as rustright-install `probe_config` and rustright-parity `derive_config`;
//   - the client reaches that instance only through the environment `run()` builds: RUSTRIGHT_BUILD_BROKER_SOCKET, RUSTRIGHT_CONTROL_ROOT and the data-root names
//     are set last, so a caller's `env` cannot point a run back at the live broker;
//   - once the instance answers, the client's own `doctor --json` must report this instance's endpoint and control root, or the instance is stopped and the start fails.
// The live broker's socket, roots and state are never named, opened or written. If no installed broker exists the start THROWS: it never skips and never fakes.
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { setTimeout as sleep } from "node:timers/promises";
import { OPT_IN_ENV, realChildProcess } from "./broker-guard.mjs";
import { validatePins } from "../broker-pins.mjs";
import { syncSnapshot } from "../snapshot.mjs";

const WINDOWS = process.platform === "win32";
const EXE = WINDOWS ? ".exe" : "";
/** `sun_path` holds 104 bytes on macOS and 108 on Linux; the config validator and the broker keep margin below that. */
const SOCKET_PATH_LIMIT = 100;
const READY_TIMEOUT_MS = Number(process.env.RK_REAL_BROKER_READY_MS) > 0 ? Number(process.env.RK_REAL_BROKER_READY_MS) : 120_000;
const STOP_GRACE_MS = 15_000;
const CLIENT_TIMEOUT_MS = 10 * 60_000;

/** Interface names that would point the client or the gate at a root or socket other than the isolated one. Removed from every environment this module builds. */
const ROOT_NAMES = ["CONTROL_ROOT", "DATA_ROOT", "MANAGED_ROOT", "EXTERNAL_ROOT", "BUILD_BROKER_SOCKET", "RUNTIME_ROOT"];
const POINTING_ENV = new RegExp(`^(?:RUSTRIGHT|RIGHTKIT)_(?:${ROOT_NAMES.join("|")})$`, "i");

let installed;

/** Parses the JSON object a `doctor --json` answer prints (the exit status is 1 while the broker is not READY, and the report is still printed). */
function parseDoctor(result, what) {
  if (result.error) throw new Error(`${what}: cannot run the client: ${result.error.message}`);
  let report;
  try {
    report = JSON.parse(result.stdout);
  } catch {
    throw new Error(`${what}: \`doctor --json\` printed no JSON (status ${result.status}): ${String(result.stderr || result.stdout).slice(-400)}`);
  }
  if (!report || typeof report !== "object") throw new Error(`${what}: \`doctor --json\` did not print an object`);
  return report;
}

const isFile = (file) => {
  try {
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
};

/**
 * The installed RustRight: client and broker binaries (from `rustright doctor --json`: `runtime.path`) and the host's own broker config
 * (`roots.requestedByClient.configPath`). Throws a clear error when the host has none. Cached for the process.
 */
function installedRustRight() {
  if (installed) return installed;
  const what = "no installed RustRight broker on this host";
  const probe = realChildProcess.spawnSync("rustright", ["doctor", "--json", "--no-scan"], { encoding: "utf8", timeout: 60_000, windowsHide: true });
  if (probe.error?.code === "ENOENT") throw new Error(`${what}: the \`rustright\` client is not on PATH. These tests run against the installed broker and never skip or simulate it; install RustRight (tools/install).`);
  const report = parseDoctor(probe, what);
  const runtimeDir = report.runtime?.path;
  if (typeof runtimeDir !== "string" || report.runtime?.binaryPresent !== true) {
    throw new Error(`${what}: \`rustright doctor --json\` reports no installed runtime (runtime.path ${JSON.stringify(runtimeDir ?? null)}, binaryPresent ${JSON.stringify(report.runtime?.binaryPresent ?? null)}, error ${JSON.stringify(report.runtime?.error ?? null)}).`);
  }
  const real = (file) => {
    try {
      return fs.realpathSync(file);
    } catch {
      return file;
    }
  };
  const clientPath = real(path.join(runtimeDir, `rustright${EXE}`));
  // macOS installs the broker inside its signed app bundle (fixed path, GB-36); other hosts keep it beside the client.
  const bundled = path.join(runtimeDir, "RustRight Broker.app", "Contents", "MacOS", "rustright-broker");
  const brokerPath = real(process.platform === "darwin" && isFile(bundled) ? bundled : path.join(runtimeDir, `rustright-broker${EXE}`));
  for (const [label, file] of [["client", clientPath], ["broker", brokerPath]]) {
    if (!isFile(file)) throw new Error(`${what}: the installed ${label} binary is missing at ${file} (runtime ${runtimeDir}).`);
  }
  // The installed broker's config: what doctor names, else RUSTRIGHT_CONFIG, else the managed default the live broker is started with
  // (macOS: ~/Library/Application Support/RightKit/build-control/config.json; Windows: %APPDATA%\RightKit\build-control\config.json).
  const defaultConfig = process.platform === "darwin"
    ? path.join(os.homedir(), "Library", "Application Support", "RightKit", "build-control", "config.json")
    : process.platform === "win32" && process.env.APPDATA ? path.join(process.env.APPDATA, "RightKit", "build-control", "config.json") : null;
  const configPath = report.roots?.requestedByClient?.configPath ?? process.env.RUSTRIGHT_CONFIG ?? defaultConfig;
  if (typeof configPath !== "string" || !isFile(configPath)) throw new Error(`${what}: the host's broker config is not readable at ${JSON.stringify(configPath ?? null)}; the isolated instance is derived from it.`);
  let liveConfig;
  try {
    liveConfig = JSON.parse(fs.readFileSync(configPath, "utf8").replace(/^﻿/, ""));
  } catch (error) {
    throw new Error(`${what}: the host's broker config ${configPath} is not JSON: ${error.message}`);
  }
  if (!liveConfig || typeof liveConfig !== "object" || typeof liveConfig.broker !== "object") throw new Error(`${what}: the host's broker config ${configPath} has no broker section.`);
  installed = Object.freeze({ clientPath, brokerPath, runtimeDir, configPath, liveConfig, doctor: report });
  return installed;
}

/** Throws a clear error when the host has no installed broker (client and runtime binaries and a readable host config). Returns the installed paths. */
export function requireRealBroker() {
  const { clientPath, brokerPath, runtimeDir, configPath } = installedRustRight();
  return { clientPath, brokerPath, runtimeDir, configPath };
}

async function freeLoopbackPort() {
  for (let attempt = 0; attempt < 16; attempt += 1) {
    const port = await new Promise((resolve, reject) => {
      const server = net.createServer();
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => {
        const { port: bound } = server.address();
        server.close(() => resolve(bound));
      });
    });
    if (port !== 4227) return port;
  }
  throw new Error("no free sccache gateway port was found");
}

/** The isolated instance's temp root: under RIGHTKIT_SCRATCH_ROOT or os.tmpdir(); on POSIX a short fallback when the socket path would not fit `sun_path`. */
function makeRoot(name) {
  const tag = `rrb-${name.slice(0, 8)}-`;
  const bases = [process.env.RIGHTKIT_SCRATCH_ROOT || os.tmpdir()];
  if (!WINDOWS) bases.push("/tmp");
  for (const base of bases) {
    fs.mkdirSync(base, { recursive: true });
    const root = fs.realpathSync(fs.mkdtempSync(path.join(base, tag)));
    if (WINDOWS || path.join(root, "c", "broker.sock").length < SOCKET_PATH_LIMIT) return root;
    fs.rmSync(root, { recursive: true, force: true });
  }
  throw new Error(`no temp base keeps the isolated broker socket under ${SOCKET_PATH_LIMIT} bytes (tried ${bases.join(", ")})`);
}

/** The host's installed config with every writable location moved under `root`. The result is what `rustright-broker <config>` is started with. */
function isolatedConfig(live, { root, control, data, endpoint, gatewayPort }) {
  const config = structuredClone(live);
  delete config.nodePath; // legacy
  delete config.class1; // its paths name live roots
  const external = live.broker.externalRoot ?? {};
  config.broker = {
    externalRoot: { path: data, volumeId: "unknown", mounted: true, writable: true, internal: external.internal ?? false },
    capacityBytes: live.broker.capacityBytes,
    controlRoot: control,
    stateFile: path.join(control, "state", "broker.json"),
    activationFencePath: path.join(control, "state", "runtime-activation.json"), // absent on disk: no ACTIVATION_PENDING
    receiptsDir: path.join(data, "receipts"),
    tempRoot: path.join(root, "t"),
  };
  delete config.unixSocket;
  delete config.windowsPipe;
  if (WINDOWS) config.windowsPipe = endpoint;
  else config.unixSocket = endpoint;
  // The shared sccache gateway is a live resource: off in the isolated instance, and its port is never the live one.
  const sccache = { ...(config.sccache ?? {}) };
  for (const key of ["clientSide", "lifecycleOwner", "requireOwnershipProof", "expectedVersion"]) delete sccache[key];
  sccache.mode = "disabled";
  sccache.gatewayPort = gatewayPort;
  config.sccache = sccache;
  config.defaults = { ...(config.defaults ?? {}), cacheMode: "disabled" };
  return config;
}

/** The environment for a client or broker of one instance: the caller's environment minus every name that could point elsewhere, plus the instance's roots. */
function isolatedEnv(base, { control, data, endpoint }) {
  const env = {};
  for (const [key, value] of Object.entries(base)) if (!POINTING_ENV.test(key) && value !== undefined) env[key] = value;
  return {
    ...env,
    RUSTRIGHT_CONTROL_ROOT: control,
    RUSTRIGHT_DATA_ROOT: data,
    RUSTRIGHT_MANAGED_ROOT: data,
    RUSTRIGHT_EXTERNAL_ROOT: data,
    RUSTRIGHT_BUILD_BROKER_SOCKET: endpoint,
    // The gate's own resolver reads the RIGHTKIT spellings of the two roots.
    RIGHTKIT_CONTROL_ROOT: control,
    RIGHTKIT_DATA_ROOT: data,
  };
}

const live = new Set();
let exitHooked = false;
function hookExit() {
  if (exitHooked) return;
  exitHooked = true;
  process.on("exit", () => {
    for (const instance of live) instance.killSync();
  });
}

/**
 * Starts the installed RustRight broker as an isolated instance and waits for READY.
 * @param {{ name: string }} options `name` labels the instance's temp root (letters, digits, `-`, up to 24).
 */
export async function startRealBroker({ name } = {}) {
  if (typeof name !== "string" || !/^[A-Za-z0-9-]{1,24}$/.test(name)) throw new Error(`startRealBroker needs a name of letters, digits and "-" (up to 24); got ${JSON.stringify(name ?? null)}`);
  const rr = installedRustRight();
  hookExit();

  const root = makeRoot(name);
  const control = path.join(root, "c");
  const data = path.join(root, "d");
  const logs = path.join(root, "logs");
  const receiptsDir = path.join(data, "receipts");
  const endpoint = WINDOWS ? `\\\\.\\pipe\\rightkit-rrb-${crypto.randomBytes(6).toString("hex")}` : path.join(control, "broker.sock");
  const socket = endpoint;
  for (const dir of [control, path.join(control, "state"), data, receiptsDir, logs, path.join(root, "t")]) fs.mkdirSync(dir, { recursive: true, mode: 0o700 });

  let child;
  let exited = false;
  const killSync = () => {
    if (!child?.pid || exited) return;
    try {
      if (WINDOWS) realChildProcess.spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" });
      else process.kill(child.pid, "SIGKILL");
    } catch {
      // already gone
    }
  };
  const removeRoot = () => fs.rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  const instance = { killSync };
  live.add(instance);
  const tail = (file) => {
    try {
      return fs.readFileSync(file, "utf8").slice(-1200);
    } catch {
      return "";
    }
  };

  try {
    const gatewayPort = await freeLoopbackPort();
    const config = isolatedConfig(rr.liveConfig, { root, control, data, endpoint, gatewayPort });
    const configPath = path.join(control, "config.json");
    fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });

    const env = isolatedEnv(process.env, { control, data, endpoint });
    // The attestation key the broker signs receipts with: this instance's own, never the host's. Never overwrites, never prints the secret.
    const keygen = realChildProcess.spawnSync(rr.brokerPath, ["--generate-attestation-key", control], { encoding: "utf8", env, cwd: root, timeout: 60_000, windowsHide: true });
    if (keygen.error || keygen.status !== 0) throw new Error(`isolated broker: --generate-attestation-key failed (${keygen.error?.message ?? `status ${keygen.status}`}): ${String(keygen.stderr).slice(-400)}`);
    // The enrolment record the broker prints is public (key id and public key); only those two fields are read, and the key file itself never is.
    let enrolment;
    try {
      const printed = JSON.parse(keygen.stdout);
      enrolment = { keyId: printed.keyId, publicKey: printed.publicKey };
    } catch {
      throw new Error(`isolated broker: --generate-attestation-key printed no enrolment record: ${String(keygen.stdout).slice(-200)}`);
    }
    let readyReport = null;

    const out = fs.openSync(path.join(logs, "broker.stdout.log"), "a");
    const err = fs.openSync(path.join(logs, "broker.stderr.log"), "a");
    try {
      child = realChildProcess.spawn(rr.brokerPath, [configPath], { cwd: root, env, stdio: ["ignore", out, err], windowsHide: true });
    } finally {
      fs.closeSync(out);
      fs.closeSync(err);
    }
    child.once("exit", () => {
      exited = true;
    });
    const spawnFailure = new Promise((resolve) => child.once("error", resolve));

    const client = (args, options = {}) => {
      return realChildProcess.spawnSync(rr.clientPath, args, {
        cwd: options.cwd,
        env: baseEnv(options.env),
        encoding: "utf8",
        timeout: options.timeout ?? CLIENT_TIMEOUT_MS,
        windowsHide: true,
        maxBuffer: 256 * 1024 * 1024,
      });
    };
    // The environment of one client run: process.env, then the caller's, with every pointing name replaced by this instance's (so the caller cannot override it).
    function baseEnv(extra) {
      return isolatedEnv({ ...process.env, ...(extra ?? {}) }, { control, data, endpoint });
    }

    const deadline = Date.now() + READY_TIMEOUT_MS;
    let last = "no answer yet";
    for (;;) {
      if (exited || child.exitCode !== null) throw new Error(`isolated broker exited before READY (code ${child.exitCode}); stderr tail: ${tail(path.join(logs, "broker.stderr.log"))}`);
      const failure = await Promise.race([spawnFailure, sleep(0).then(() => null)]);
      if (failure) throw new Error(`isolated broker could not be started: ${failure.message}`);
      const probe = client(["doctor", "--json", "--no-scan"], { timeout: 30_000 });
      try {
        const report = parseDoctor(probe, "isolated broker");
        last = `phase ${JSON.stringify(report.broker?.phase ?? null)}, reachable ${report.broker?.reachable}, error ${JSON.stringify(report.broker?.error ?? null)}`;
        if (report.broker?.reachable === true && report.broker?.phase === "READY") {
          const reportedControl = report.roots?.requestedByClient?.controlRoot;
          const same = (a, b) => typeof a === "string" && path.resolve(a) === path.resolve(b);
          const realOf = (p) => {
            try {
              return fs.realpathSync(p);
            } catch {
              return p;
            }
          };
          // The endpoint must be this instance's; a broker version whose doctor reports no control root is checked by endpoint alone.
          if (report.broker.endpoint !== endpoint || (reportedControl !== undefined && !same(realOf(reportedControl), realOf(control)))) {
            throw new Error(`isolation check failed: the client reports endpoint ${JSON.stringify(report.broker.endpoint)} and control root ${JSON.stringify(reportedControl)}, expected ${endpoint} and ${control}`);
          }
          readyReport = report;
          break;
        }
      } catch (error) {
        if (/isolation check failed/.test(error.message)) throw error;
        last = error.message;
      }
      if (Date.now() > deadline) throw new Error(`isolated broker was not READY within ${READY_TIMEOUT_MS} ms (${last}); stderr tail: ${tail(path.join(logs, "broker.stderr.log"))}`);
      await sleep(250);
    }

    // Starting an instance is the test's opt-in (broker-guard.mjs OPT_IN_ENV). The production seams under test read process.env, so it is pointed at THIS
    // instance (endpoint and roots set last, live ones removed) for as long as the instance runs, and restored by stop(). Each test file is its own
    // node --test process, so this never reaches another file or the live broker.
    const savedEnv = { ...process.env };
    for (const key of Object.keys(process.env)) delete process.env[key];
    Object.assign(process.env, baseEnv(), { [OPT_IN_ENV]: "1" });

    let stopped;
    const stop = async () => {
      stopped ??= (async () => {
        for (const key of Object.keys(process.env)) delete process.env[key];
        Object.assign(process.env, savedEnv);
        live.delete(instance);
        if (child && !exited && child.exitCode === null) {
          const gone = new Promise((resolve) => child.once("exit", resolve));
          if (WINDOWS) killSync();
          else {
            try {
              child.kill("SIGTERM");
            } catch {
              // already gone
            }
            const timer = sleep(STOP_GRACE_MS).then(() => "timeout");
            if ((await Promise.race([gone, timer])) === "timeout") killSync();
          }
          await Promise.race([gone, sleep(STOP_GRACE_MS)]);
        }
        removeRoot();
      })();
      return stopped;
    };

    // The pins the gate verifies receipts against (broker-pins.mjs): this instance's attestation key, this host's name (what the broker signs as `host`: its
    // hostname) and the broker digest of the runtime that is answering. Validated by the gate's own validatePins; the secret key is never read.
    // The enrolled digest is the sha256 of the broker executable that runs (as pins/broker-<platform>.json records it); doctor fields when present.
    const brokerDigest = readyReport.runtime?.expectedBrokerDigest ?? readyReport.answeringBroker?.handshakeRuntime?.brokerDigest
      ?? crypto.createHash("sha256").update(fs.readFileSync(rr.brokerPath)).digest("hex");
    const pins = validatePins({ host: WINDOWS ? process.env.COMPUTERNAME : os.hostname(), keyId: enrolment.keyId, publicKey: enrolment.publicKey, brokerDigest }, "the isolated broker's enrolment");
    const snapshotsRoot = path.join(data, "snapshots");

    return {
      clientPath: rr.clientPath,
      pins,
      /** A production-synced snapshot of `commit` of the checkout `repo` under this instance's data root (`<data>/snapshots/<repo key>`); removed by stop() with the root. */
      snapshot({ repo, commit }) {
        if (typeof repo !== "string" || typeof commit !== "string") throw new Error("snapshot({ repo, commit }) needs a repository path and a commit");
        // The temp root is not on a managed volume: the device guard is told "/" and the data root are different devices, nothing else about the sync is changed.
        const device = (p) => (path.resolve(p) === path.parse(path.resolve(p)).root ? 1n : 2n);
        const synced = syncSnapshot({ checkout: repo, rev: commit, root: snapshotsRoot, managedRoot: data, device });
        return { dir: synced.path, commit: synced.commit, tree: synced.tree };
      },
      env: baseEnv(),
      socket,
      controlRoot: control,
      dataRoot: data,
      receiptsDir,
      run(args, options = {}) {
        if (!Array.isArray(args) || args.some((arg) => typeof arg !== "string")) throw new Error("run(args): args must be an array of strings");
        const result = client(args, options);
        if (result.error) throw new Error(`run ${JSON.stringify(args)}: cannot run the client: ${result.error.message}`);
        return { status: result.status ?? (result.signal ? 128 : null), stdout: result.stdout ?? "", stderr: result.stderr ?? "" };
      },
      stop,
    };
  } catch (error) {
    live.delete(instance);
    killSync();
    try {
      removeRoot();
    } catch {
      // the OS temp sweep removes what remains
    }
    throw error;
  }
}
