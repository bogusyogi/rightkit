#!/usr/bin/env node

import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WORKSPACE = path.resolve(HERE, "../../..");
const PHASE5_CRATES = ["rightkit-onnx", "rightkit-ocr", "rightkit-ffmpeg"];
const execFileAsync = promisify(execFile);

// null = not on this machine. The Windows runtime copies are staged only on Windows and the mac
// binaries only on a Mac, so a gate that threw on a missing file could only ever run on one OS —
// which is exactly the OS whose evidence it is not supposed to be trusting.
async function sha256(file) {
  try {
    return createHash("sha256").update(await readFile(file)).digest("hex");
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
}





async function cargoManifests(root) {
  const { stdout } = await execFileAsync(
    "git",
    ["-C", root, "ls-files", "--", "Cargo.toml", "**/Cargo.toml"],
    { encoding: "utf8", windowsHide: true },
  );
  return stdout
    .split(/\r?\n/)
    .filter(Boolean)
    .map((relative) => path.join(root, relative));
}

async function prematureDependencies() {
  const hits = [];
  for (const repo of ["coderight", "scraperight", "heardright"]) {
    for (const manifest of await cargoManifests(path.join(WORKSPACE, repo))) {
      const text = await readFile(manifest, "utf8");
      for (const crate of PHASE5_CRATES) {
        if (new RegExp(`^\\s*${crate}\\s*=`, "m").test(text)) {
          hits.push({ repo, crate, manifest: path.relative(WORKSPACE, manifest) });
        }
      }
    }
  }
  return hits;
}

async function crateStates() {
  const states = {};
  for (const name of PHASE5_CRATES) {
    const cargo = path.join(WORKSPACE, "rightkit/crates", name, "Cargo.toml");
    const text = await readFile(cargo, "utf8");
    states[name] = { publish: !/^publish\s*=\s*false\s*$/m.test(text) };
  }
  return states;
}

// Reads only rightkit's own media contract and the vendored artifacts; it needs no consumer checkout.
export async function inspectMedia() {
  const contract = JSON.parse(
    await readFile(path.join(WORKSPACE, "rightkit/characterization/media/contract.json"), "utf8"),
  );
  const windows = contract.artifacts.find((artifact) => artifact.id === "ffmpeg-7.1.5-windows-x64");
  if (!windows) throw new Error("approved Windows FFmpeg characterization is missing");
  const copies = await Promise.all(
    windows.locations.map(async ({ path: relative }) => ({
      path: relative,
      sha256: await sha256(path.join(WORKSPACE, relative)),
    })),
  );
  const present = copies.filter(({ sha256: digest }) => digest !== null);

  // The mac rows carry a sha256 but nothing was checking it, so their status was an assertion.
  // Hash them wherever the binary is actually on disk. `location` is written with a `~/Claude`
  // prefix in the contract; only the vendored path under the workspace is machine-independent.
  const macos = await Promise.all(
    contract.artifacts
      .filter((artifact) => artifact.os === "macos")
      .map(async ({ arch, status, sha256: expected, location }) => {
        const relative = `vendor/ffmpeg-lgpl/${path.basename(location ?? "")}`;
        const actual = await sha256(path.join(WORKSPACE, relative));
        return {
          arch,
          status,
          present: actual !== null,
          sha256Matches: actual === null ? null : actual === expected,
        };
      }),
  );

  return {
    windows: {
      id: windows.id,
      status: windows.status,
      sha256: windows.sha256,
      // Every copy we can see matches. Copies staged only on the other OS are reported, not
      // silently counted as verified.
      allCopiesMatch: present.every(({ sha256: digest }) => digest === windows.sha256),
      missingCopies: copies.filter(({ sha256: digest }) => digest === null).map(({ path: p }) => p),
    },
    macos,
  };
}

export async function inspectPhase5Adoption() {
  // The OCR section of this gate is gone (2026-07-18). It existed to keep
  // ScrapeRight and CodeRight on byte-identical PP-OCRv6 source + weights after
  // a hand-placed v6 upgrade in one app silently diverged from the other. Both
  // apps have since moved to OS-native OCR (Apple Vision / Windows.Media.Ocr)
  // and deleted their PP-OCR pipelines, so there is no shared OCR core and no
  // model artifact left to compare. rightkit-ocr has no consumers; retire the
  // crate rather than reviving this check.
  const [crates, dependencies, media] = await Promise.all([
    crateStates(),
    prematureDependencies(),
    inspectMedia(),
  ]);
  return {
    schema: "rightkit.phase5-adoption-readiness.v1",
    crates,
    prematureDependencies: dependencies,
    media,
    readyToPublish:
      dependencies.length === 0 &&
      Object.values(crates).every(({ publish }) => publish) &&
      media.macos.every(({ status: artifactStatus }) => artifactStatus === "approved"),
  };
}

const isMain = process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url;
if (isMain) {
  inspectPhase5Adoption()
    .then((report) => process.stdout.write(`${JSON.stringify(report, null, 2)}\n`))
    .catch((error) => {
      process.stderr.write(`${error.message}\n`);
      process.exitCode = 1;
    });
}
