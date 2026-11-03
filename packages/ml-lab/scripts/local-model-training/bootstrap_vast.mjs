#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const DEFAULT_RECIPES = join(SCRIPT_DIR, "model-recipes.json");
const DEFAULT_TOKENIZER_PROBE = join(SCRIPT_DIR, "preflight_exact_tokenizers.py");
const RUNTIME_PYTHON = process.env.RIGHTKIT_ML_PYTHON || "python3";
export const LLAMA_CPP_COMMIT = "fb0e6b621917488d623437349fb5361e0ac21c70";
export const LLAMA_CPP_REMOTE = "https://github.com/ggml-org/llama.cpp.git";
export const PYTORCH_IMAGE = "pytorch/pytorch:2.7.1-cuda12.6-cudnn9-devel@sha256:639b8229ccfd8a3aa803cf49c33d6d6fe406750d79aaf723fe8c0eb1060d8cff";
export const RUNTIME_SOURCES = {
  pytorch_image_manifest: "https://registry-1.docker.io/v2/pytorch/pytorch/manifests/2.7.1-cuda12.6-cudnn9-devel",
  unsloth_pypi: "https://pypi.org/project/unsloth/2026.7.4/",
  torchao_wheel_index: "https://download.pytorch.org/whl/cu126/torchao/",
  llama_cpp_release: "https://github.com/ggml-org/llama.cpp/releases/tag/b10082",
  llama_cpp_repository: LLAMA_CPP_REMOTE,
};
const REQUIRED_PINS = ["torch==2.7.1", "torchvision==0.22.1", "unsloth==2026.7.4", "unsloth_zoo==2026.7.4", "transformers==5.5.0", "trl==0.24.0", "peft==0.19.1", "datasets==4.3.0", "bitsandbytes==0.49.2", "accelerate==1.14.0", "xformers==0.0.31.post1", "torchao @ https://download.pytorch.org/whl/cu126/torchao-0.13.0%2Bcu126-cp39-abi3-manylinux_2_24_x86_64.manylinux_2_28_x86_64.whl"];

const fail = (message) => { throw new Error(`Vast bootstrap: ${message}`); };
const shellQuote = (value) => /^[A-Za-z0-9_./:@=+%,-]+$/.test(value) ? value : JSON.stringify(value);
const display = (argv) => argv.map((value) => shellQuote(String(value))).join(" ");
const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;

export async function validateRuntimeInputs({ requirements, containerImage = PYTORCH_IMAGE, outputDir, llamaCppDir, orchestrator, orchestratorArgs = [], recipes = DEFAULT_RECIPES, tokenizerProbe = DEFAULT_TOKENIZER_PROBE }) {
  if (!requirements) fail("--requirements is required");
  if (!outputDir) fail("--output-dir is required");
  if (containerImage !== PYTORCH_IMAGE) fail(`container image must equal the approved immutable digest ${PYTORCH_IMAGE}; received ${containerImage}`);
  const absoluteRequirements = resolve(requirements);
  const contents = await readFile(absoluteRequirements, "utf8").catch(() => fail(`requirements file does not exist: ${absoluteRequirements}`));
  const lines = new Set(contents.split(/\r?\n/).map((line) => line.trim()).filter((line) => line && !line.startsWith("#")));
  for (const pin of REQUIRED_PINS) if (!lines.has(pin)) fail(`requirements file is missing required pin: ${pin}`);
  const absoluteRecipes = resolve(recipes), absoluteTokenizerProbe = resolve(tokenizerProbe);
  if (!existsSync(absoluteRecipes) || !existsSync(absoluteTokenizerProbe)) fail("exact tokenizer preflight recipes or probe script is missing");
  if (!Array.isArray(orchestratorArgs) || !orchestratorArgs.every((argument) => typeof argument === "string")) fail("orchestrator arguments must be a JSON array of strings");
  const absoluteOutputDir = resolve(outputDir);
  const absoluteLlamaCppDir = resolve(llamaCppDir || join(absoluteOutputDir, "tools", "llama.cpp"));
  return {
    requirements: absoluteRequirements,
    requirementsSha256: sha256(contents),
    containerImage,
    outputDir: absoluteOutputDir,
    llamaCppDir: absoluteLlamaCppDir,
    buildDir: resolve(absoluteLlamaCppDir, "build-heardright-cuda"),
    receiptPath: join(absoluteOutputDir, "vast-runtime-receipt.json"),
    recipes: absoluteRecipes,
    tokenizerProbe: absoluteTokenizerProbe,
    tokenizerReceiptPath: join(absoluteOutputDir, "exact-tokenizer-preflight.json"),
    llamaCppCommit: LLAMA_CPP_COMMIT,
    orchestrator: orchestrator ? resolve(orchestrator) : null,
    orchestratorArgs,
  };
}

export function buildBootstrapPlan(inputs) {
  const converter = join(inputs.llamaCppDir, "convert_hf_to_gguf.py");
  const cli = join(inputs.buildDir, "bin", "llama-cli");
  return {
    converter,
    cli,
    commands: [
      `${display([RUNTIME_PYTHON, "-m", "pip", "install", "--no-cache-dir", "--upgrade", "-r", inputs.requirements])} && ${display([RUNTIME_PYTHON, "-m", "pip", "check"])}`,
      display([RUNTIME_PYTHON, inputs.tokenizerProbe, "--recipes", inputs.recipes, "--output", inputs.tokenizerReceiptPath]),
      `${display(["git", "clone", "--filter=blob:none", LLAMA_CPP_REMOTE, inputs.llamaCppDir])} && ${display(["git", "-C", inputs.llamaCppDir, "checkout", LLAMA_CPP_COMMIT])}`,
      display(["cmake", "-S", inputs.llamaCppDir, "-B", inputs.buildDir, "-DGGML_CUDA=ON", "-DLLAMA_CURL=OFF"]),
      display(["cmake", "--build", inputs.buildDir, "--target", "llama-cli", "--parallel"]),
    ],
  };
}

function run(argv, { cwd, capture = false } = {}) {
  const result = spawnSync(argv[0], argv.slice(1), { cwd, encoding: "utf8", stdio: capture ? ["ignore", "pipe", "pipe"] : "inherit" });
  if (result.error) fail(`could not execute ${display(argv)}: ${result.error.message}`);
  if (result.status !== 0) fail(`command failed (${result.status}): ${display(argv)}${capture ? `\n${result.stderr || ""}` : ""}`);
  return capture ? (result.stdout || "").trim() : "";
}

function findCli(buildDir) {
  for (const candidate of [join(buildDir, "bin", "llama-cli"), join(buildDir, "bin", "llama-cli.exe")]) if (existsSync(candidate)) return candidate;
  return null;
}

function ensureLlamaCpp(inputs) {
  if (existsSync(inputs.llamaCppDir)) {
    const current = run(["git", "-C", inputs.llamaCppDir, "rev-parse", "HEAD"], { capture: true });
    if (current !== LLAMA_CPP_COMMIT) fail(`existing llama.cpp checkout is ${current}, expected ${LLAMA_CPP_COMMIT}; refusing to alter it`);
  } else {
    run(["git", "clone", "--filter=blob:none", LLAMA_CPP_REMOTE, inputs.llamaCppDir]);
    run(["git", "-C", inputs.llamaCppDir, "checkout", LLAMA_CPP_COMMIT]);
  }
  const converter = join(inputs.llamaCppDir, "convert_hf_to_gguf.py");
  if (!existsSync(converter)) fail(`pinned llama.cpp checkout has no converter at ${converter}`);
  let cli = findCli(inputs.buildDir);
  if (!cli) {
    run(["cmake", "-S", inputs.llamaCppDir, "-B", inputs.buildDir, "-DGGML_CUDA=ON", "-DLLAMA_CURL=OFF"]);
    run(["cmake", "--build", inputs.buildDir, "--target", "llama-cli", "--parallel"]);
    cli = findCli(inputs.buildDir);
  }
  if (!cli) fail(`llama-cli build did not produce an executable under ${inputs.buildDir}/bin`);
  return { converter, cli };
}

function parseArgs(argv) {
  const values = { requirements: join(SCRIPT_DIR, "requirements.vast-cu126.pins.txt"), recipes: DEFAULT_RECIPES, tokenizerProbe: DEFAULT_TOKENIZER_PROBE, containerImage: PYTORCH_IMAGE, dryRun: false, invokeOrchestrator: false, orchestratorArgs: [] };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--dry-run") values.dryRun = true;
    else if (argument === "--invoke-orchestrator") values.invokeOrchestrator = true;
    else if (["--requirements", "--recipes", "--tokenizer-probe", "--container-image", "--output-dir", "--llama-cpp-dir", "--orchestrator", "--orchestrator-args-json"].includes(argument)) {
      const value = argv[++index];
      if (!value) fail(`${argument} requires a value`);
      if (argument === "--requirements") values.requirements = value;
      else if (argument === "--recipes") values.recipes = value;
      else if (argument === "--tokenizer-probe") values.tokenizerProbe = value;
      else if (argument === "--container-image") values.containerImage = value;
      else if (argument === "--output-dir") values.outputDir = value;
      else if (argument === "--llama-cpp-dir") values.llamaCppDir = value;
      else if (argument === "--orchestrator") values.orchestrator = value;
      else { try { values.orchestratorArgs = JSON.parse(value); } catch { fail("--orchestrator-args-json must be valid JSON"); } }
    } else fail(`unknown argument ${argument}`);
  }
  if (values.invokeOrchestrator && !values.orchestrator) fail("--invoke-orchestrator requires --orchestrator");
  return values;
}

async function writeReceipt(inputs, runtime, tokenizerCompatibility) {
  const receipt = {
    schema_version: 2,
    created_at: new Date().toISOString(),
    sources: RUNTIME_SOURCES,
    container_image: inputs.containerImage,
    requirements_path: inputs.requirements,
    requirements_sha256: inputs.requirementsSha256,
    llama_cpp: { remote: LLAMA_CPP_REMOTE, commit: LLAMA_CPP_COMMIT, converter: runtime.converter, cli: runtime.cli },
    exact_tokenizer_preflight: tokenizerCompatibility,
    runtime: {
      python_version: run([RUNTIME_PYTHON, "--version"], { capture: true }),
      nvidia_smi: run(["nvidia-smi", "--query-gpu=name,driver_version,memory.total", "--format=csv,noheader"], { capture: true }),
      pip_freeze: run([RUNTIME_PYTHON, "-m", "pip", "freeze", "--all"], { capture: true }).split("\n"),
    },
  };
  await writeFile(inputs.receiptPath, `${JSON.stringify(receipt, null, 2)}\n`);
  return inputs.receiptPath;
}

export async function main(argv = process.argv.slice(2)) {
  const options = parseArgs(argv);
  const inputs = await validateRuntimeInputs(options);
  const plan = buildBootstrapPlan(inputs);
  if (options.dryRun) {
    process.stdout.write(`${JSON.stringify({ mode: "dry-run", image: inputs.containerImage, requirements_sha256: inputs.requirementsSha256, llama_cpp_commit: LLAMA_CPP_COMMIT, sources: RUNTIME_SOURCES, plan }, null, 2)}\n`);
    return;
  }
  await mkdir(inputs.outputDir, { recursive: true });
  run([RUNTIME_PYTHON, "-m", "pip", "install", "--no-cache-dir", "--upgrade", "-r", inputs.requirements]);
  run([RUNTIME_PYTHON, "-m", "pip", "check"]);
  run([RUNTIME_PYTHON, inputs.tokenizerProbe, "--recipes", inputs.recipes, "--output", inputs.tokenizerReceiptPath]);
  const tokenizerReceipt = JSON.parse(await readFile(inputs.tokenizerReceiptPath, "utf8"));
  const auditedTokenizers = Array.isArray(tokenizerReceipt.candidates) && tokenizerReceipt.candidates.length === 6 && tokenizerReceipt.candidates.every((item) => item.integer_token_sequence === true && Number.isInteger(item.encoded_tokens) && item.encoded_tokens > 0 && Number.isInteger(item.token_id_min) && Number.isInteger(item.token_id_max) && /^sha256:[0-9a-f]{64}$/.test(item.input_ids_sha256 ?? ""));
  if (tokenizerReceipt.schema_version !== "heardright.l1.exact_tokenizer_preflight.v1" || tokenizerReceipt.status !== "passed" || tokenizerReceipt.transformers_version !== "5.5.0" || tokenizerReceipt.candidate_count !== 6 || !auditedTokenizers || tokenizerReceipt.recipes_sha256 !== sha256(await readFile(inputs.recipes))) fail("exact tokenizer preflight receipt is invalid");
  const tokenizerCompatibility = { path: inputs.tokenizerReceiptPath, sha256: sha256(await readFile(inputs.tokenizerReceiptPath)), transformers_version: tokenizerReceipt.transformers_version, candidate_count: tokenizerReceipt.candidate_count };
  const runtime = ensureLlamaCpp(inputs);
  const receiptPath = await writeReceipt(inputs, runtime, tokenizerCompatibility);
  if (options.invokeOrchestrator) {
    if (!existsSync(inputs.orchestrator)) fail(`orchestrator does not exist: ${inputs.orchestrator}`);
    run([process.execPath, inputs.orchestrator, "--environment", receiptPath, "--llama-cpp-converter", runtime.converter, "--llama-cli", runtime.cli, ...inputs.orchestratorArgs]);
  }
  process.stdout.write(`${JSON.stringify({ mode: "prepared", receipt: receiptPath, converter: runtime.converter, cli: runtime.cli }, null, 2)}\n`);
}

if (process.argv[1] && !process.execArgv.includes("-e") && (process.platform === "win32" ? basename(process.argv[1]).toLowerCase() === basename(fileURLToPath(import.meta.url)).toLowerCase() : basename(process.argv[1]) === basename(fileURLToPath(import.meta.url)))) main().catch((error) => { process.stderr.write(`${error.stack || error.message}\n`); process.exitCode = 1; });
