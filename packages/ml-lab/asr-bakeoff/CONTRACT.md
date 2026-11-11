# ASR bake-off contract

One contract for every adapter under `adapters/` and for `../wer/score-asr-wer.mjs`, so a new model is
compared with the others by adding one adapter.

## Input: the clip manifest

A JSON array (or an object whose `rows`, `clips` or `items` key is that array). Each row:

| Field | Meaning |
|---|---|
| `id` | Opaque row id. Adapters see nothing else that identifies the clip. |
| `path` | Audio file (16 kHz mono WAV is the expected form; adapters resample where their model needs it). |
| `duration_s` | Clip length in seconds. |
| `sha256` | Digest of the audio file. |

The manifest carries no ground truth, no clip name, no vocabulary and no bias payload: adapters are
name-blind and call only the model's documented, unconditioned transcription path.

## Adapter command line

```sh
python3 adapters/<model>/adapter.py --manifest <manifest.json> (--dry-run | --smoke-ids id1,id2 | --full)
    [--model-dir <dir>] [--out-dir <dir>] [--device cuda|cpu] [--dtype ...] [--seed N]
```

- `--dry-run` validates the manifest and prints the resolved configuration and a memory estimate; it
  loads no weights.
- `--model-dir` defaults to `./.cache/asr-models/<model>` and `--out-dir` to `./out/<model>`, both relative
  to the working directory. Weights are never downloaded by an adapter; the model's pinned revision is
  in the adapter (`PINNED_REVISION` or the spec), and `bundle-qualify.mjs` checks a candidate's complete
  download size before anyone fetches it.
- Exit codes: 0 done, 2 usage or bad manifest, 3 model directory missing.

## Output

In `--out-dir`:

| File | Content |
|---|---|
| `hypotheses.jsonl` | One row per clip: `{id, hypothesis, decode_s, ...}`. |
| `status.json` | `completion`, `rows_done`, `rows_total`, `cold_load_s`, `peak_vram_mb`, `retries`. |
| `env.json` | Runtime record: framework and version, model revision, dtype, device and the decode settings. |

## Scoring

```sh
node ../wer/score-asr-wer.mjs --hyp out/<model>/hypotheses.jsonl --truth id_to_truth.json \
    [--terms a,b,c] [--term-group <group>] [--out scored.json]
```

`id_to_truth.json` maps each `id` to `{clip, gt, group, duration_s?}`. The scorer reports corpus WER,
WER by group and by duration bucket, latency percentiles, missing rows, and (with `--terms`) exact-term
recovery. Score only after the hypotheses are sealed.
