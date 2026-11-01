#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { platform } from "node:os";

const python = platform() === "win32" ? ["py", "-3.11"] : ["python3"];
const commands = [
  [process.execPath, "--test", "scripts/local-model-training/test_bootstrap_vast.mjs", "scripts/local-model-training/test_candidate_rejection_progression.mjs", "scripts/local-model-training/test_checkpoint_lifecycle.mjs", "scripts/local-model-training/test_evaluation_policy_v2.mjs", "scripts/local-model-training/test_finalize_l1_training_corpus.mjs", "scripts/local-model-training/test_finalize_smoke_gate.mjs", "scripts/local-model-training/test_launch_vast.mjs", "scripts/local-model-training/test_orchestrate.mjs", "scripts/local-model-training/test_orchestrator_output_dirs.mjs", "scripts/local-model-training/test_sync_checkpoints_windows.mjs", "scripts/local-model-training/test_validate_output.mjs", "scripts/local-model-training/test_score_evaluation.mjs", "scripts/local-model-training/test_protected_span_guard.mjs", "scripts/local-model-training/test_serialize_l1_parity.mjs"],
  [...python, "scripts/local-model-training/test_autotune_fullft_batch.py"],
  [...python, "scripts/local-model-training/test_wrapper_dependencies.py"],
  [...python, "scripts/local-model-training/test_prepare_source_checkpoint.py"],
  [...python, "scripts/local-model-training/test_recovered_train_contract.py"],
  [...python, "scripts/local-model-training/test_train_evaluation_normalization.py"],
  [...python, "scripts/local-model-training/test_durable_evaluation_contract.py"],
  [...python, "scripts/local-model-training/test_transformers_native_full.py"],
  [...python, "scripts/local-model-training/test_exact_tokenizers.py"],
  [...python, "scripts/local-model-training/test_exact_source_resolution.py"],
  [...python, "scripts/local-model-training/train.py", "--help"],
  [...python, "scripts/local-model-training/train_transformers_native_full.py", "--help"],
  [...python, "scripts/local-model-training/select_full_weight_backend.py", "--help"],
  [...python, "scripts/local-model-training/finalize_transformers_native_full.py", "--help"],
  [...python, "scripts/local-model-training/preflight_exact_tokenizers.py", "--help"],
  [...python, "scripts/local-model-training/resolve_exact_model_sources.py", "--help"],
  [...python, "scripts/local-model-training/evaluate_gguf.py", "--help"],
  [process.execPath, "scripts/local-model-training/finalize_smoke_gate.mjs", "--help"],
];
for (const command of commands) {
  const result = spawnSync(command[0], command.slice(1), { stdio: "inherit" });
  if (result.error || result.status !== 0) process.exit(result.status ?? 1);
}
process.stdout.write(`${JSON.stringify({ status: "ok", suites: commands.length, recovered_runner: "sha256:a911da2dabee3c9333e11f7e8cfd9baab164816b81505722e0af22cf4dfed6dd" })}\n`);
