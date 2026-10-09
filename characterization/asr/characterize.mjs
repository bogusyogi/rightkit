#!/usr/bin/env node

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const CACHE = resolve(HERE, "../../../../.cache/rightkit-asr-characterization");
export const MODEL_FILES = [
  "config.json",
  "encoder-model.onnx",
  "decoder_joint-model.int8.onnx",
  "mel_nemo.onnx",
  "mel_nemo.onnx.data",
  "tokenizer.model",
  "vocab.txt",
];

export function normalizeTranscript(value) {
  return value.trim().replace(/\s+/g, " ").toLowerCase();
}

export function parseTranscript(stdout) {
  const line = stdout
    .split(/\r?\n/)
    .find((candidate) => candidate.startsWith("TRANSCRIPT "));
  if (!line) throw new Error("runtime output did not contain a TRANSCRIPT line");
  return normalizeTranscript(line.slice("TRANSCRIPT ".length));
}

export async function sha256File(path) {
  return createHash("sha256").update(await readFile(path)).digest("hex");
}

function cargoPath(path) {
  // A drive-letter path is already absolute for Cargo on every host; posix resolve() would prefix the cwd.
  const absolute = /^[A-Za-z]:[\\/]/.test(path) ? path : resolve(path);
  return absolute.replaceAll("\\", "/").replaceAll('"', '\\"');
}

function probeSource(typeName) {
  return `use std::path::Path;

fn load_wav(path: &Path) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.channels != 1 {
        return Err(format!("fixture must be mono, got {} channels", spec.channels).into());
    }
    let samples = match spec.sample_format {
        hound::SampleFormat::Int => reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?
            .into_iter().map(|sample| sample as f32 / 32768.0).collect(),
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
    };
    Ok(resample_to_16k(&samples, spec.sample_rate))
}

fn resample_to_16k(input: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == 16_000 || input.is_empty() {
        return input.to_vec();
    }
    let output_len = ((input.len() as u64 * 16_000 + source_rate as u64 / 2)
        / source_rate as u64) as usize;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * source_rate as f64 / 16_000.0;
            let left = position.floor() as usize;
            let right = (left + 1).min(input.len() - 1);
            let fraction = (position - left as f64) as f32;
            input[left] * (1.0 - fraction) + input[right] * fraction
        })
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let model_dir = args.get(1).ok_or("missing model directory")?;
    let fixture = args.get(2).ok_or("missing WAV fixture")?;
    let config = parakeet_rs::ExecutionConfig::new()
        .with_execution_provider(parakeet_rs::ExecutionProvider::DirectML);
    let mut runtime = parakeet_rs::${typeName}::load(model_dir, Some(config))?;
    let result = runtime.transcribe_result(&load_wav(Path::new(fixture))?)?;
    println!("TRANSCRIPT {}", result.text.trim());
    Ok(())
}
`;
}

export function buildCargoWorkspace({ heardRightCrate, scrapeRightCrate }) {
  const files = new Map();
  for (const [name, cratePath, typeName] of [
    ["heardright", heardRightCrate, "HrTransducer"],
    ["scraperight", scrapeRightCrate, "ScrapeRightTransducer"],
  ]) {
    files.set(
      `${name}/Cargo.toml`,
      `[package]\nname = "rightkit-asr-char-${name}"\nversion = "0.0.0"\nedition = "2021"\npublish = false\n\n[dependencies]\nhound = "3.5"\nparakeet-rs = { path = "${cargoPath(cratePath)}", features = ["directml", "load-dynamic"] }\n`,
    );
    files.set(`${name}/src/main.rs`, probeSource(typeName));
  }
  return files;
}

async function materialize(files) {
  const workspace = join(CACHE, "workspace");
  await rm(workspace, { recursive: true, force: true });
  for (const [relative, content] of files) {
    const target = join(workspace, relative);
    await mkdir(dirname(target), { recursive: true });
    await writeFile(target, content, "utf8");
  }
  return workspace;
}

function option(args, name) {
  const index = args.indexOf(name);
  if (index < 0 || !args[index + 1]) throw new Error(`missing ${name}`);
  return args[index + 1];
}

async function main(args) {
  const heardRightCrate = option(args, "--heardright-crate");
  const scrapeRightCrate = option(args, "--scraperight-crate");
  const model = resolve(option(args, "--model"));
  const fixtures = args.flatMap((value, index) =>
    value === "--fixture" && args[index + 1] ? [resolve(args[index + 1])] : [],
  );
  if (fixtures.length === 0) throw new Error("at least one --fixture is required");

  const workspace = await materialize(
    buildCargoWorkspace({ heardRightCrate, scrapeRightCrate }),
  );
  await mkdir(join(CACHE, "target"), { recursive: true });
  const env = { ...process.env, CARGO_TARGET_DIR: join(CACHE, "target") };
  const rows = [];
  for (const fixture of fixtures) {
    const transcripts = {};
    for (const app of ["heardright", "scraperight"]) {
      const stdout = execFileSync(
        "cargo",
        ["run", "--release", "--quiet", "--", model, fixture],
        { cwd: join(workspace, app), env, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] },
      );
      transcripts[app] = parseTranscript(stdout);
    }
    rows.push({
      fixture,
      fixtureSha256: await sha256File(fixture),
      transcripts,
      equal: transcripts.heardright === transcripts.scraperight,
    });
  }
  const result = {
    schema: "rightkit.asr-characterization.v1",
    model: Object.fromEntries(
      await Promise.all(
        MODEL_FILES.map(async (name) => [name, await sha256File(join(model, name))]),
      ),
    ),
    fixtures: rows,
    parity: rows.every((row) => row.equal),
  };
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  if (!result.parity) process.exitCode = 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
