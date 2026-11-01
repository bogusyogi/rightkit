#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { basename, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { shaFile, syncOnWindows } from "./checkpoint_lifecycle.mjs";

const fail = (message) => { throw new Error(`Windows checkpoint watcher: ${message}`); };
const value = (argv, name) => {
  const index = argv.indexOf(name);
  return index < 0 ? undefined : argv[index + 1];
};

export function parseOptions(argv) {
  const remote = value(argv, "--remote");
  const remoteRunDir = value(argv, "--remote-run-dir");
  const destinationRoot = value(argv, "--destination-root");
  if (!remote || !remoteRunDir || !destinationRoot) {
    fail("--remote, --remote-run-dir, and --destination-root are required");
  }
  if (!/^[A-Za-z0-9_.@-]+$/u.test(remote) || !/^\/[A-Za-z0-9_./-]+$/u.test(remoteRunDir)) {
    fail("remote target or run path contains unsupported characters");
  }
  const pollSeconds = Number(value(argv, "--poll-seconds") ?? 10);
  if (!(pollSeconds >= 2 && pollSeconds <= 60)) {
    fail("--poll-seconds must be between 2 and 60");
  }
  const instanceId = value(argv, "--instance-id");
  const remoteBundler = value(argv, "--remote-bundler");
  if (
    (instanceId && !/^\d+$/u.test(instanceId))
    || (remoteBundler && !/^\/[A-Za-z0-9_./-]+\.py$/u.test(remoteBundler))
  ) {
    fail("instance ID or remote bundler path is invalid");
  }
  return {
    remote,
    remoteRunDir,
    destinationRoot: resolve(destinationRoot),
    pollSeconds,
    once: argv.includes("--once"),
    instanceId,
    remoteBundler,
  };
}

const TRANSPORT_OPTIONS = [
  "-o", "BatchMode=yes",
  "-o", "ConnectTimeout=15",
  "-o", "ServerAliveInterval=15",
  "-o", "ServerAliveCountMax=2",
];

export function transportInvocation(command, args, capture = false) {
  return {
    args: command === "ssh" || command === "scp" ? [...TRANSPORT_OPTIONS, ...args] : args,
    stdio: ["ignore", capture ? "pipe" : "inherit", "inherit"],
  };
}

function run(command, args, capture = false) {
  const invocation = transportInvocation(command, args, capture);
  const result = spawnSync(command, invocation.args, {
    encoding: capture ? "utf8" : undefined,
    stdio: invocation.stdio,
  });
  if (result.error || result.status !== 0) {
    fail(`${command} failed with status ${result.status ?? "launch-error"}`);
  }
  return capture ? result.stdout : "";
}

const sha = shaFile;
const terminalStatuses = new Set([
  "complete",
  "complete_with_rejections",
  "completed_no_viable_candidates",
  "failed",
]);
export const isTerminalStatus = (status) => terminalStatuses.has(status);
export const laneFromGgufReceipt = (path) => {
  const match = /^([a-z0-9_]+)--gguf-evaluate\.json$/u.exec(basename(path));
  return match?.[1] ?? null;
};
const atomic = (path, payload) => {
  const temporary = `${path}.tmp-${process.pid}`;
  writeFileSync(temporary, `${JSON.stringify(payload, null, 2)}\n`);
  renameSync(temporary, path);
};

function syncTerminalRun(options) {
  if (!options.instanceId || !options.remoteBundler) return null;
  let state;
  try {
    state = JSON.parse(run("ssh", [options.remote, "cat", `${options.remoteRunDir}/state.json`], true));
  } catch {
    return null;
  }
  if (!isTerminalStatus(state.status)) return null;
  const finalRoot = resolve(options.destinationRoot, "_final");
  const localArchive = resolve(finalRoot, "run-artifacts.tar.gz");
  const localManifest = resolve(finalRoot, "run-artifacts.manifest.json");
  const localReceipt = resolve(finalRoot, "final-run.receipt.json");
  mkdirSync(finalRoot, { recursive: true });
  if (!existsSync(localReceipt)) {
    const remoteArchive = `${options.remoteRunDir}-final-artifacts.tar.gz`;
    const remoteManifest = `${options.remoteRunDir}-final-artifacts.manifest.json`;
    run("ssh", [
      options.remote,
      "python3",
      options.remoteBundler,
      "--run-root",
      options.remoteRunDir,
      "--archive",
      remoteArchive,
      "--manifest",
      remoteManifest,
    ]);
    run("scp", [`${options.remote}:${remoteArchive}`, `${localArchive}.partial`]);
    run("scp", [`${options.remote}:${remoteManifest}`, `${localManifest}.partial`]);
    renameSync(`${localArchive}.partial`, localArchive);
    renameSync(`${localManifest}.partial`, localManifest);
    const manifest = JSON.parse(readFileSync(localManifest, "utf8"));
    if (
      manifest.schema_version !== "heardright.l1.final_run_transfer.v1"
      || manifest.terminal_status !== state.status
      || manifest.archive_sha256 !== sha(localArchive)
    ) {
      fail("terminal artifact transfer failed manifest/hash verification");
    }
    atomic(localReceipt, {
      schema_version: "heardright.l1.final_run_windows_receipt.v1",
      verified_on_windows: true,
      terminal_status: state.status,
      archive: localArchive,
      archive_sha256: manifest.archive_sha256,
      archive_bytes: manifest.archive_bytes,
      manifest: localManifest,
      manifest_sha256: sha(localManifest),
      state_sha256: manifest.state_sha256,
      file_count: manifest.file_count,
      verified_at: new Date().toISOString(),
    });
  }
  const remoteReceipt = `${options.remoteRunDir}/sync-receipts/final-run.receipt.json`;
  const temporary = `${remoteReceipt}.tmp-${process.pid}`;
  run("ssh", [options.remote, "mkdir", "-p", `${options.remoteRunDir}/sync-receipts`]);
  run("scp", [localReceipt, `${options.remote}:${temporary}`]);
  run("ssh", [options.remote, "mv", "--", temporary, remoteReceipt]);
  run("vastai", ["stop", "instance", options.instanceId]);
  return {
    status: state.status,
    receipt: localReceipt,
    instanceStopped: options.instanceId,
  };
}

function syncCompletedLanes(options) {
  if (!options.remoteBundler) return 0;
  const listing = run("ssh", [
    options.remote,
    "find",
    `${options.remoteRunDir}/stage-receipts`,
    "-maxdepth",
    "1",
    "-type",
    "f",
    "-name",
    "*--gguf-evaluate.json",
    "-print",
  ], true).split(/\r?\n/u).filter(Boolean).sort();
  const remoteSynced = new Set(
    run("ssh", [
      options.remote,
      "find",
      `${options.remoteRunDir}/sync-receipts`,
      "-maxdepth",
      "1",
      "-type",
      "f",
      "-name",
      "*-artifacts.receipt.json",
      "-print",
    ], true).split(/\r?\n/u).filter(Boolean).map((path) => basename(path)),
  );
  let synced = 0;
  for (const remoteReceipt of listing) {
    const lane = laneFromGgufReceipt(remoteReceipt);
    if (!lane) continue;
    if (remoteSynced.has(`${lane}-artifacts.receipt.json`)) {
      synced += 1;
      continue;
    }
    const laneRoot = resolve(options.destinationRoot, "_artifacts", lane);
    const localArchive = resolve(laneRoot, `${lane}.tar.gz`);
    const localManifest = resolve(laneRoot, `${lane}.manifest.json`);
    const localReceipt = resolve(laneRoot, `${lane}.receipt.json`);
    mkdirSync(laneRoot, { recursive: true });
    if (!existsSync(localReceipt)) {
      const remoteArchive = `${options.remoteRunDir}-${lane}-artifacts.tar.gz`;
      const remoteManifest = `${options.remoteRunDir}-${lane}-artifacts.manifest.json`;
      run("ssh", [
        options.remote,
        "python3",
        options.remoteBundler,
        "--run-root",
        options.remoteRunDir,
        "--lane",
        lane,
        "--archive",
        remoteArchive,
        "--manifest",
        remoteManifest,
      ]);
      run("scp", [`${options.remote}:${remoteArchive}`, `${localArchive}.partial`]);
      run("scp", [`${options.remote}:${remoteManifest}`, `${localManifest}.partial`]);
      renameSync(`${localArchive}.partial`, localArchive);
      renameSync(`${localManifest}.partial`, localManifest);
      const manifest = JSON.parse(readFileSync(localManifest, "utf8"));
      if (
        manifest.schema_version !== "heardright.l1.lane_transfer.v1"
        || manifest.lane !== lane
        || manifest.archive_sha256 !== sha(localArchive)
      ) {
        fail(`lane artifact transfer failed manifest/hash verification: ${lane}`);
      }
      atomic(localReceipt, {
        schema_version: "heardright.l1.lane_windows_receipt.v1",
        verified_on_windows: true,
        lane,
        archive: localArchive,
        archive_sha256: manifest.archive_sha256,
        archive_bytes: manifest.archive_bytes,
        manifest: localManifest,
        manifest_sha256: sha(localManifest),
        state_sha256: manifest.state_sha256,
        file_count: manifest.file_count,
        verified_at: new Date().toISOString(),
      });
    }
    const remoteFinal = `${options.remoteRunDir}/sync-receipts/${lane}-artifacts.receipt.json`;
    const temporary = `${remoteFinal}.tmp-${process.pid}`;
    run("scp", [localReceipt, `${options.remote}:${temporary}`]);
    run("ssh", [options.remote, "mv", "--", temporary, remoteFinal]);
    synced += 1;
  }
  return synced;
}

function service(options) {
  const requestRoot = resolve(options.destinationRoot, "_requests");
  const receiptRoot = resolve(options.destinationRoot, "_receipts");
  mkdirSync(requestRoot, { recursive: true });
  mkdirSync(receiptRoot, { recursive: true });
  run("ssh", [
    options.remote,
    "mkdir",
    "-p",
    `${options.remoteRunDir}/sync-requests`,
    `${options.remoteRunDir}/sync-receipts`,
  ]);
  const listing = run("ssh", [
    options.remote,
    "find",
    `${options.remoteRunDir}/sync-requests`,
    "-maxdepth",
    "1",
    "-type",
    "f",
    "-name",
    "*.request.json",
    "-print",
  ], true).split(/\r?\n/u).filter(Boolean).sort();
  for (const remoteRequest of listing) {
    const name = basename(remoteRequest);
    const localRequest = resolve(requestRoot, name);
    const localReceipt = resolve(
      receiptRoot,
      name.replace(/\.request\.json$/u, ".receipt.json"),
    );
    if (!existsSync(localRequest)) {
      run("scp", [`${options.remote}:${remoteRequest}`, localRequest]);
    }
    if (!existsSync(localReceipt)) {
      syncOnWindows({
        requestPath: localRequest,
        destinationRoot: options.destinationRoot,
        receiptPath: localReceipt,
        remote: options.remote,
      });
    }
  }
  return {
    requests: listing.length,
    artifactReceipts: syncCompletedLanes(options),
    terminal: syncTerminalRun(options),
  };
}

export async function main(argv = process.argv.slice(2)) {
  if (process.platform !== "win32") fail("the watcher must run on Windows");
  const options = parseOptions(argv);
  for (;;) {
    const result = service(options);
    process.stdout.write(`${JSON.stringify({
      status: "watching",
      ...result,
      at: new Date().toISOString(),
    })}\n`);
    if (result.terminal || options.once) return;
    await new Promise((done) => setTimeout(done, options.pollSeconds * 1000));
  }
}

if (
  process.argv[1]
  && !process.execArgv.includes("-e")
  && (
    process.platform === "win32"
      ? basename(process.argv[1]).toLowerCase() === basename(fileURLToPath(import.meta.url)).toLowerCase()
      : basename(process.argv[1]) === basename(fileURLToPath(import.meta.url))
  )
) {
  main().catch((error) => {
    process.stderr.write(`${error.stack || error.message}\n`);
    process.exitCode = 1;
  });
}
