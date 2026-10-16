#!/usr/bin/env node

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdtemp,
  readFile,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_WORKSPACE = path.resolve(HERE, "../../..");
const MODEL_FILES = ["det.onnx", "rec.onnx", "en_dict.txt"];

export function extractOcrOutput(stdout) {
  const lines = stdout.replaceAll("\r\n", "\n").split("\n");
  const start = lines.findIndex((line) => line.trim() === "=== EXTRACTED TEXT ===");
  if (start < 0) {
    throw new Error("OCR output marker was not emitted");
  }
  const end = lines.findIndex(
    (line, index) => index > start && /^=+$/.test(line.trim()),
  );
  if (end < 0) {
    throw new Error("OCR output closing marker was not emitted");
  }
  return lines.slice(start + 1, end).join("\n").trim();
}

export async function loadContract(contractUrl = new URL("contract.json", import.meta.url)) {
  return JSON.parse(await readFile(contractUrl, "utf8"));
}

/** The native-library catalog: the single source of the ONNX Runtime digest. */
export const NATIVE_CATALOG_URL = new URL("../../crates/rightkit-ffmpeg/catalog/native-libraries.json", import.meta.url);

export async function loadNativeCatalog(catalogUrl = NATIVE_CATALOG_URL) {
  return JSON.parse(await readFile(catalogUrl, "utf8"));
}

/**
 * The SHA-256 of the ONNX Runtime file for `platform`, from the catalog's one approved or pinned
 * `onnxruntime` record for that platform. Only Windows x64 is characterized.
 */
export function expectedRuntimeDigest(catalog, platform = os.platform()) {
  const records = platform === "win32"
    ? (catalog.libraries ?? []).filter((r) => r.library === "onnxruntime" && r.os === "windows" && r.arch === "x86_64" && (r.status === "approved" || r.status === "pinned") && r.sha256)
    : [];
  if (records.length !== 1) {
    throw new Error(`No characterized ORT runtime digest for ${platform}`);
  }
  return records[0].sha256.toLowerCase();
}

async function artifactBytes(value) {
  if (Buffer.isBuffer(value)) return value;
  return readFile(value);
}

export async function verifyArtifactSet(artifacts, expectedDigests) {
  const verified = {};
  for (const [name, expected] of expectedDigests) {
    const bytes = await artifactBytes(artifacts.get(name));
    const actual = createHash("sha256").update(bytes).digest("hex");
    if (actual !== expected.toLowerCase()) {
      throw new Error(`${name} digest mismatch: expected ${expected}, got ${actual}`);
    }
    verified[name] = actual;
  }
  return verified;
}

function option(args, name) {
  const index = args.indexOf(name);
  return index < 0 ? undefined : args[index + 1];
}

function defaults(workspace) {
  const local = process.env.LOCALAPPDATA;
  return {
    coderightRepo: path.join(workspace, "coderight"),
    scraperightRepo: path.join(workspace, "scraperight"),
    coderightModelDir: local
      ? path.join(local, "CodeRight", "ocr-model")
      : undefined,
    scraperightModelDir: local
      ? path.join(local, "ScrapeRight", "models", "ocr")
      : undefined,
    coderightOrt: path.join(
      workspace,
      "coderight",
      "apps",
      "coderight-tauri",
      "src-tauri",
      "resources",
      "runtime",
      os.platform() === "win32" ? "onnxruntime.dll" : "libonnxruntime.dylib",
    ),
    scraperightOrt: path.join(
      workspace,
      "scraperight",
      "src-tauri",
      "resources",
      "runtime",
      os.platform() === "win32" ? "onnxruntime.dll" : "libonnxruntime.dylib",
    ),
  };
}

async function requireFile(file, label) {
  try {
    if (!(await stat(file)).isFile()) throw new Error("not a file");
  } catch {
    throw new Error(`${label} not found: ${file}`);
  }
}

function runOcr({ command, args, cwd, env }) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env: { ...process.env, ...env },
      shell: false,
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8").on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.setEncoding("utf8").on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code !== 0) {
        reject(new Error(`${command} exited ${code}\n${stderr}`));
        return;
      }
      resolve({ stdout, stderr });
    });
  });
}

export async function characterize(args = process.argv.slice(2)) {
  const workspace = path.resolve(option(args, "--workspace") ?? DEFAULT_WORKSPACE);
  const fallback = defaults(workspace);
  const config = {
    ...fallback,
    coderightModelDir:
      option(args, "--coderight-model-dir") ?? fallback.coderightModelDir,
    scraperightModelDir:
      option(args, "--scraperight-model-dir") ?? fallback.scraperightModelDir,
    coderightOrt: option(args, "--coderight-ort") ?? fallback.coderightOrt,
    scraperightOrt: option(args, "--scraperight-ort") ?? fallback.scraperightOrt,
  };
  if (!config.coderightModelDir || !config.scraperightModelDir) {
    throw new Error(
      "Model locations are required outside Windows; pass --coderight-model-dir and --scraperight-model-dir",
    );
  }

  const contract = await loadContract();
  const fixtureBase64 = (
    await readFile(path.join(HERE, contract.fixture.file), "utf8")
  ).trim();
  const fixtureBytes = Buffer.from(fixtureBase64, "base64");
  await verifyArtifactSet(
    new Map([["fixture", fixtureBytes]]),
    new Map([["fixture", contract.fixture.sha256]]),
  );

  const modelArtifacts = (directory) =>
    new Map(MODEL_FILES.map((name) => [name, path.join(directory, name)]));
  const modelDigests = new Map(Object.entries(contract.models));
  const [coderightModels, scraperightModels] = await Promise.all([
    verifyArtifactSet(modelArtifacts(config.coderightModelDir), modelDigests),
    verifyArtifactSet(modelArtifacts(config.scraperightModelDir), modelDigests),
  ]);

  await Promise.all([
    requireFile(config.coderightOrt, "CodeRight ORT runtime"),
    requireFile(config.scraperightOrt, "ScrapeRight ORT runtime"),
  ]);
  const runtimeArtifacts = new Map([
    ["coderight", config.coderightOrt],
    ["scraperight", config.scraperightOrt],
  ]);
  const runtimeHashes = {};
  for (const [name, file] of runtimeArtifacts) {
    runtimeHashes[name] = createHash("sha256")
      .update(await readFile(file))
      .digest("hex");
  }
  if (runtimeHashes.coderight !== runtimeHashes.scraperight) {
    throw new Error(
      `ORT runtime digest mismatch: CodeRight ${runtimeHashes.coderight}, ScrapeRight ${runtimeHashes.scraperight}`,
    );
  }
  const expectedOrt = expectedRuntimeDigest(await loadNativeCatalog());
  if (runtimeHashes.coderight !== expectedOrt) {
    throw new Error(
      `ORT runtime digest changed: expected ${expectedOrt}, got ${runtimeHashes.coderight}`,
    );
  }

  const temporary = await mkdtemp(path.join(os.tmpdir(), "rightkit-ocr-"));
  const fixture = path.join(temporary, "rightkit-ocr-parity.png");
  try {
    await writeFile(fixture, fixtureBytes);
    const [coderight, scraperight] = await Promise.all([
      runOcr({
        command: "cargo",
        args: ["run", "--quiet", "-p", "coderight-ocr", "--bin", "ocr_test", "--", fixture],
        cwd: path.join(config.coderightRepo, "engine"),
        env: {
          CODERIGHT_OCR_MODEL_DIR: config.coderightModelDir,
          ORT_DYLIB_PATH: config.coderightOrt,
        },
      }),
      runOcr({
        command: "cargo",
        args: ["run", "--quiet", "-p", "scraperight-ocr", "--bin", "ocr_test", "--", fixture],
        cwd: config.scraperightRepo,
        env: {
          SCRAPERIGHT_OCR_MODEL_DIR: config.scraperightModelDir,
          ORT_DYLIB_PATH: config.scraperightOrt,
        },
      }),
    ]);
    const coderightOutput = extractOcrOutput(coderight.stdout);
    const scraperightOutput = extractOcrOutput(scraperight.stdout);
    if (coderightOutput !== scraperightOutput) {
      throw new Error(
        `OCR output mismatch\nCodeRight:\n${coderightOutput}\nScrapeRight:\n${scraperightOutput}`,
      );
    }
    if (coderightOutput !== contract.expectedOutput) {
      throw new Error(
        `OCR output changed from the characterized contract\nExpected:\n${contract.expectedOutput}\nActual:\n${coderightOutput}`,
      );
    }
    return {
      schemaVersion: contract.schemaVersion,
      fixtureSha256: contract.fixture.sha256,
      modelDigests: coderightModels,
      scraperightModelDigests: scraperightModels,
      ortRuntimeSha256: runtimeHashes.coderight,
      output: coderightOutput,
      parity: true,
    };
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

const isMain =
  process.argv[1] &&
  pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url;
if (isMain) {
  characterize()
    .then((result) => {
      process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
    })
    .catch((error) => {
      process.stderr.write(`${error.message}\n`);
      process.exitCode = 1;
    });
}
