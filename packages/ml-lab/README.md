# @rightkit/ml-lab

Scripts shipped as files, not an importable library: nothing is built and there is no `main`.

- `scripts/`: Node (`.mjs`) and Python tooling for ASR bake-offs (`open-asr-bakeoff`), CoreML and
  ONNX conversion (`coreml`, `parakeet-v3-streaming`), local-model training and screening
  (`local-model-training`, `local-model-screening`), polish replay and bias experiments.
- `bakeoff/`: per-experiment evaluation harnesses and corpus builders.
- `tools/`: wake-word corpus conditioning, source-truth audit and ASR verification scripts
  (Python, PowerShell, shell).

Moved from HeardRight (`tauri-app-next/scripts`, `tauri-app-next/bakeoff`, `scripts`). The tree
keeps its original relative layout, so scripts that call one another (for example the `.mjs`
orchestrators and the `.py` training steps beside them) still resolve. Run them from the
`@rightkit/ml-lab` package root.

Environment switches were renamed from `HEARDRIGHT_*` / `HR_*` to `RIGHTKIT_ML_*`. On-disk schema
ids (`heardright.l1.*`) are unchanged because existing data carries them. Some scripts still hold
machine-specific absolute paths from their original authors; set their documented variables or edit
them before use. No model, corpus or dataset is shipped.
