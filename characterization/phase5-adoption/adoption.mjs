#!/usr/bin/env node

import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WORKSPACE = path.resolve(HERE, "../../../..");
const SHARED_CORE = [
  "support/boxes.rs",
  "support/decode.rs",
  "support/preprocess.rs",
];
const PHASE5_CRATES = ["rightkit-ort", "rightkit-ocr", "rightkit-media"];
const OCR_ARTIFACTS = ["det.onnx", "rec.onnx", "en_dict.txt"];
const OCR_ARTIFACT_ROOTS = {
  coderight: "coderight/apps/coderight-tauri/src-tauri/resources/ocr-model",
  scraperight: "scraperight/models/ocr",
};
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

async function requireMatch(file, pattern, label) {
  const text = await readFile(file, "utf8");
  const match = text.match(pattern);
  if (!match) throw new Error(`${label} not found in ${path.relative(WORKSPACE, file)}`);
  return match;
}

async function inspectOcrCore() {
  const roots = {
    shared: path.join(WORKSPACE, "tools/rightkit/crates/rightkit-ocr/src"),
    coderight: path.join(WORKSPACE, "coderight/engine/crates/ocr/src"),
    scraperight: path.join(WORKSPACE, "scraperight/crates/ocr/src"),
  };
  const coreDrift = [];
  for (const relative of SHARED_CORE) {
    const hashes = Object.fromEntries(
      await Promise.all(
        Object.entries(roots).map(async ([name, root]) => [name, await sha256(path.join(root, relative))]),
      ),
    );
    if (new Set(Object.values(hashes)).size !== 1) coreDrift.push({ file: relative, hashes });
  }
  return coreDrift;
}

async function inspectAdapters() {
  const definitions = {
    coderight: {
      modelFile: path.join(WORKSPACE, "coderight/engine/crates/ocr/src/support/model_files.rs"),
      cargoFile: path.join(WORKSPACE, "coderight/engine/crates/ocr/Cargo.toml"),
      modelEnv: "CODERIGHT_OCR_MODEL_DIR",
      modelSuffix: ["CodeRight", "models", "ocr"],
    },
    scraperight: {
      modelFile: path.join(WORKSPACE, "scraperight/crates/ocr/src/support/model_files.rs"),
      cargoFile: path.join(WORKSPACE, "scraperight/crates/ocr/Cargo.toml"),
      modelEnv: "SCRAPERIGHT_OCR_MODEL_DIR",
      modelSuffix: ["ScrapeRight", "models", "ocr"],
    },
  };
  const adapters = {};
  for (const [app, definition] of Object.entries(definitions)) {
    const modelText = await readFile(definition.modelFile, "utf8");
    if (!modelText.includes(`\"${definition.modelEnv}\"`)) {
      throw new Error(`${app} OCR model environment adapter drifted`);
    }
    let cursor = 0;
    for (const segment of definition.modelSuffix) {
      cursor = modelText.indexOf(`join(\"${segment}\")`, cursor);
      if (cursor < 0) throw new Error(`${app} OCR model suffix drifted at ${segment}`);
      cursor += segment.length;
    }
    const [, ortCrate] = await requireMatch(
      definition.cargoFile,
      /^([a-z0-9-]+-ort-common)\s*=/m,
      `${app} ORT adapter dependency`,
    );
    adapters[app] = {
      modelEnv: definition.modelEnv,
      modelSuffix: definition.modelSuffix,
      ortCrate,
    };
  }
  return adapters;
}

// The shared-core check above hashes source only. Phase 5's gate also requires "the same
// crate/model digest", so the deployed model artifacts must be hashed too — a status string in
// PROVENANCE.toml is not evidence that the recorded digests are what the apps actually ship.
async function inspectOcrArtifacts(provenance) {
  const recorded = new Map();
  for (const [, file, digest] of provenance.matchAll(
    /^file\s*=\s*"([^"]+)"\s*\r?\nsha256\s*=\s*"([0-9a-f]{64})"/gm,
  )) {
    recorded.set(file, digest);
  }

  const drift = [];
  for (const file of OCR_ARTIFACTS) {
    const deployed = Object.fromEntries(
      await Promise.all(
        Object.entries(OCR_ARTIFACT_ROOTS).map(async ([app, root]) => [
          app,
          await sha256(path.join(WORKSPACE, root, file)),
        ]),
      ),
    );
    const expected = recorded.get(file) ?? null;
    const appsAgree = new Set(Object.values(deployed)).size === 1;
    const matchesRecord =
      expected !== null && Object.values(deployed).every((digest) => digest === expected);
    if (!appsAgree || !matchesRecord) {
      drift.push({ file, recorded: expected, ...deployed });
    }
  }
  return drift;
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
    const cargo = path.join(WORKSPACE, "tools/rightkit/crates", name, "Cargo.toml");
    const text = await readFile(cargo, "utf8");
    states[name] = { publish: !/^publish\s*=\s*false\s*$/m.test(text) };
  }
  return states;
}

async function inspectMedia() {
  const contract = JSON.parse(
    await readFile(path.join(WORKSPACE, "tools/rightkit/characterization/media/contract.json"), "utf8"),
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
  const provenance = await readFile(
    path.join(WORKSPACE, "tools/rightkit/crates/rightkit-ocr/PROVENANCE.toml"),
    "utf8",
  );
  const status = provenance.match(/^status\s*=\s*"([^"]+)"/m)?.[1];
  if (!status) throw new Error("OCR provenance status is missing");

  const [coreDrift, artifactDrift, adapters, crates, dependencies, media] = await Promise.all([
    inspectOcrCore(),
    inspectOcrArtifacts(provenance),
    inspectAdapters(),
    crateStates(),
    prematureDependencies(),
    inspectMedia(),
  ]);
  return {
    schema: "rightkit.phase5-adoption-readiness.v1",
    ocr: { coreDrift, artifactDrift, adapters, provenanceStatus: status },
    crates,
    prematureDependencies: dependencies,
    media,
    readyToPublish:
      coreDrift.length === 0 &&
      artifactDrift.length === 0 &&
      dependencies.length === 0 &&
      status === "resolved" &&
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
