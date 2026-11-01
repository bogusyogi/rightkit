import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { LLAMA_CPP_COMMIT, PYTORCH_IMAGE, buildBootstrapPlan, validateRuntimeInputs } from "./bootstrap_vast.mjs";

const PINS = ["torch==2.7.1", "torchvision==0.22.1", "unsloth==2026.7.4", "unsloth_zoo==2026.7.4", "transformers==5.5.0", "trl==0.24.0", "peft==0.19.1", "datasets==4.3.0", "bitsandbytes==0.49.2", "accelerate==1.14.0", "xformers==0.0.31.post1", "torchao @ https://download.pytorch.org/whl/cu126/torchao-0.13.0%2Bcu126-cp39-abi3-manylinux_2_24_x86_64.manylinux_2_28_x86_64.whl"];

async function fixtureRequirements() {
  const root = await mkdtemp(join(tmpdir(), "heardright-vast-bootstrap-"));
  const file = join(root, "requirements.txt");
  await writeFile(file, `${PINS.join("\n")}\n`);
  return { root, file };
}

test("validates the immutable container and pinned llama.cpp plan", async () => {
  const { root, file } = await fixtureRequirements();
  try {
    const inputs = await validateRuntimeInputs({ requirements: file, containerImage: PYTORCH_IMAGE, outputDir: join(root, "out") });
    const plan = buildBootstrapPlan(inputs);
    assert.equal(inputs.llamaCppCommit, LLAMA_CPP_COMMIT);
    assert.match(plan.commands.join("\n"), new RegExp(`git clone .*${LLAMA_CPP_COMMIT}`));
    assert.match(plan.commands.join("\n"), /cmake -S/);
    assert.match(plan.commands.join("\n"), /--target llama-cli/);
    assert.match(plan.commands[1], /preflight_exact_tokenizers\.py/);
    assert.ok(plan.commands[1].includes("--recipes"));
    assert.match(plan.converter, /convert_hf_to_gguf\.py$/);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("rejects a mutable container tag and a missing pin", async () => {
  const { root, file } = await fixtureRequirements();
  try {
    await assert.rejects(validateRuntimeInputs({ requirements: file, containerImage: "pytorch/pytorch:2.7.1-cuda12.6-cudnn9-devel", outputDir: join(root, "out") }), /digest/);
    await writeFile(file, "torch==2.7.1\n");
    await assert.rejects(validateRuntimeInputs({ requirements: file, containerImage: PYTORCH_IMAGE, outputDir: join(root, "out") }), /missing required pin/i);
  } finally { await rm(root, { recursive: true, force: true }); }
});
