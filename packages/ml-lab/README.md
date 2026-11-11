# @rightkit/ml-lab

Private ASR model tooling for comparing speech-recognition models and preparing them for Apple's Neural
Engine. Not published to npm and not an app dependency: a developer runs it from a checkout. It holds no
model, corpus, dataset or machine path; every input is a command-line argument and every default is
relative to the working directory.

| Item | Value |
|---|---|
| Where it runs | A developer machine: Node 22+ for the `.mjs` tools, Python 3.11 for the adapters and CoreML scripts |
| Version | 0.2.0 (private) |
| License | MIT OR Apache-2.0 |

## Entry points

| Entry | What it does |
|---|---|
| `wer/score-asr-wer.mjs` (bin `rk-asr-wer`, import `@rightkit/ml-lab/wer`) | Scores `hypotheses.jsonl` against `id_to_truth.json`: corpus WER, WER by group and duration bucket, latency percentiles, missing rows, optional exact-term recovery (`--terms`, `--term-group`). `--self-test` runs its hand-checked cases. |
| `asr-bakeoff/CONTRACT.md` | The one input and output contract every adapter and the scorer follow. |
| `asr-bakeoff/adapters/<model>/adapter.py` | Name-blind inference adapters, each pinned to one model revision: ARK-ASR 0.6B and 3B, Canary 1B Flash, Canary-Qwen 2.5B, Cohere Transcribe, Granite Speech 4.1 2B, Higgs Audio v3 STT, MOSS Transcribe 2B, Nemotron 3.5 ASR 0.6B, Qwen3-ASR 1.7B. `--dry-run` validates a manifest without loading weights. |
| `asr-bakeoff/bundle-qualify.mjs` (bin `rk-bundle-qualify`, import `@rightkit/ml-lab/bundle-qualify`) | Checks a candidate's complete download (weights, tokenizer, vocoder, runtime) against a byte cap from sizes alone, before anything is downloaded. |
| `coreml/convert_parakeet_coreml.py` | Parakeet TDT v3 or RNN-T to four static-shape, palettized CoreML stages (runs on CUDA Linux or WSL2). |
| `coreml/export_tdt_decoder_matrix_mac.py` | Decoder and joint precision matrix for Parakeet TDT v3. |
| `coreml/coreml_bench.py` | Loads the converted stages on an Apple Silicon Mac and reports real-time factor, first-token latency and WER. |
| `coreml/vad/convert_silero_vad_coreml.py` | Silero VAD 16 kHz ONNX to CoreML (with its PyTorch twin `silero_torch.py`). |
| `coreml/convert_whisper_coreml.sh` | Whisper to CoreML through `whisperkittools`, for custom quantizations only. |

## Python environment

`requirements/asr-adapters.txt` and `requirements/coreml.txt` list what each half imports. Install them
into a dedicated virtual environment; RightKit's build and CI never install them. Each adapter run records
its framework version in `out/<model>/env.json`.

## Not here

Dated experiment folders, corpus builders, training and screening pipelines, wake-word corpus tools and
scripts with machine paths stayed with HeardRight (see CHANGELOG 0.2.0). Only tooling that applies to any
app comparing or converting ASR models belongs in this package.

## Testing

```sh
pnpm --dir packages/ml-lab test
```
