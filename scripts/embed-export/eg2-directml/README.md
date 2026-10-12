# EmbeddingGemma 2 export for DirectML (draft)

Builds a half-precision, fixed-shape, standard-operator ONNX export of the EmbeddingGemma 2 text encoder that gives correct output on DirectML (variant E40). It is the outcome of comparing 40 export variants on one reference machine (RTX 4070 Laptop 8 GB, Windows 11).

## Source (pinned)

| File | Bytes | SHA-256 |
|---|---|---|
| `onnx-community/embeddinggemma-2-ONNX` @ `daa72c51243991dfcaf9f9137d2c573d8f7790c0`, `onnx/model.onnx` | 421,421 | `bc47de15f81208a5c99e5ab10f746d5e33b51ea228b7dc0bef9c133a94f1c1c3` |
| same revision, `onnx/model.onnx_data` | 1,084,170,240 | `9fd452bfc6916e5a92f080f8dada35598ca4512b21c65897d4c5974d5f9630a2` |
| `tokenizer.json` (local copy, not compared with the repository listing) | 32,170,510 | `4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4` |

Step 1 derives the text-only FP32 graph from that graph and writes `<output-dir>/onnx/model_text.onnx` (SHA-256 `70bfde2da286ad8c8d37447828f0ba99cd73eb7a17a0f76ab3a969b76160bdcf`) plus `<output-dir>/onnx/model_text.onnx_data`, a renamed copy of the upstream weights file (same bytes, same SHA-256), an `export-receipt.json` and a `qualification-input-contract.json`.

## Commands

Third-party imports and the versions used:

| Package | Version | Used by |
|---|---|---|
| onnx | 1.21.0 | all build scripts |
| onnxruntime (CPU) | 1.25.1 | build verification, FP32 CPU reference |
| onnxruntime-directml | 1.24.4 | DirectML measurements and the DirectML zero check |
| numpy | 1.26.4 | all |
| tokenizers | 0.22.2 (0.23.3 in the DirectML environment) | verification, `bench.py` |
| psutil | 7.2.2 | `bench.py` |
| torch | 2.6.0+cu124 | `bench.py`, cuda and tensorrt providers only (for its CUDA DLLs) |

The CPU reference was computed on onnxruntime 1.25.1 while the DirectML runs used 1.24.4; that difference is deliberate (two installs), not an error. Everything else is the standard library. No path has a default; every one is an argument.

```
py -3.11 scripts\source\export_text_only.py --source-dir <upstream onnx folder> --output-dir <text-only folder>
py -3.11 scripts\groupE\build_eps.py --variant E40-fp16-prescale-gelu1-foldgamma-rope4-embeds-fp32tail --src <text-only folder>\onnx\model_text.onnx --tokenizer <tokenizer.json> --ref <fp32-cpu-ref.json> --out <output root>
py -3.11 bench.py --model <text-only folder>\onnx\model_text.onnx --tokenizer <tokenizer.json> --provider cpu --tag fp32-cpu-ref --work-dir <work folder>
py -3.11 scripts\groupE\zero_check.py --dir <output root>\<variant> --lengths 32,128 --provider dml --device-id 0
```

The build writes eight files into `<output root>\<variant>\`: five graphs, `model.onnx_data`, `embed_tokens.f16.npy` and `manifest.json`. The third command produces the `--ref` file as `<work folder>\results\fp32-cpu-ref.json`; the fourth needs onnxruntime-directml.

`--ref` is the FP32 CPU reference used by the built-in verification (vectors for one fixed passage at 32/64/128/256/512 tokens, produced by `bench.py --provider cpu` on the text-only graph). The fp16 token table is derived from the source graph's embedding initializer. Both commands were run from this folder: the first reproduced the text-only graph byte for byte from the pinned upstream graph (`model_text.onnx` SHA-256 `70bfde2d...`, weights file unchanged), and the second reproduced the reference machine's export byte for byte (the five graphs, the weights file and the token table: seven files, hashes below). `manifest.json` is not byte-stable, because it records the build time in seconds. It names the source graph by file name and SHA-256, never by path. In the last rebuild from this folder it was 10,973 bytes, SHA-256 `35c1744a9c822dbfbd64a720f4bf1f4fed0e63647f9147a37eaaa5ac157f5ceb`.

Expected output hashes (SHA-256):

| File | Bytes | SHA-256 |
|---|---|---|
| `model_32.onnx` | 416,097 | `8e1aaddd28ca5437e2da0da5f9dac5ea54766dc26b0bb760f104f5963c3bb418` |
| `model_64.onnx` | 416,179 | `bcc8b63ea05c9963130cd2995be013f8dd516e40fc6bdac81b1313ea6ca5c2cc` |
| `model_128.onnx` | 417,636 | `7afb92bf83831e3c496a993555f82f85afa9b107b77ef13297aa12d2f9d7e1f8` |
| `model_256.onnx` | 417,638 | `1874b05842faf3c9b3badc6261765d95ad809a0ec2afc6f7dbb647aa161f8ce0` |
| `model_512.onnx` | 417,641 | `f6fd91519cf1023da0adcedeab9fb7c77b01913f84ed647bb79d7f839994b741` |
| `model.onnx_data` | 290,203,648 | `c398505134b2bb717becc109c00408ecd9faef0a8418981a549653aea6bb73ac` |
| `embed_tokens.f16.npy` | 268,435,584 | `b574d03a53a4cfbcf76d07dc291d2ffe614ca7720c79cb655ec338f583ee53e8` |

## What the scripts do outside their arguments

- `build_eps.py`, `build_e.py`, `build_d.py`: refuse to run when `<output root>\<variant>\` already exists (exit status 1) unless `--overwrite` is passed; with `--overwrite` they delete that folder and rebuild it. They lower their own process priority on Windows (BELOW_NORMAL, through ctypes). They write nothing outside `<output root>\<variant>\`.
- `bench.py`: runs `nvidia-smi` to read GPU memory (the result is recorded; a failure is recorded, not fatal) and writes `profiles\` and `results\` under the required `--work-dir`, plus `trt-cache\` there when the tensorrt provider is used. It writes nothing beside itself.
- `export_text_only.py`: writes only under `--output-dir`.
- No script uses the network.

## What the export does

- Replaces the `com.microsoft` RotaryEmbedding operator and the two layer-norm contrib operators with `ai.onnx` operators only. The output is opset 21, IR version 10, default domain only, with the `com.microsoft` import dropped. This was read from `opset_import` of each of the five exported graphs (`[('', 21)]` in all five; every one of the 1,376 nodes is in the default domain). The scripts do not set the opset; it is inherited from the source graph. RotaryEmbedding is what gives wrong output on DirectML: with only that operator replaced, cosine against the CPU reference goes from 0.57-0.73 to 1.0000; replacing only Gelu or only the norms does not help.
- RMS norm as `Mul(1/64) -> LpNormalization(p=2, axis=-1) -> scale`; the scale is folded into the following MatMul weights where the norm feeds only MatMuls. The prescale keeps the sum of squares inside fp16 range (a plain fp16 `x*x` norm overflows on DirectML: min cosine 0.934). No epsilon in these norms.
- Gelu as one `Gelu` node in the erf form (the source uses the tanh form).
- RoPE as `x*COS + Gather(x, perm)*SIN`, four nodes per application, constants precomputed per length.
- fp16 weights and activations; the tail runs in fp32 (see "NaN diagnosis").
- One file per fixed length (32, 64, 128, 256, 512), batch 1, sharing one weights file. 1,376 nodes at length 128. Short-input latency on DirectML tracks node count (about 5 microseconds per node on a synthetic 1,000-node chain), which is why the graph is minimised.

## NaN diagnosis

An all-zero `inputs_embeds` returned NaN on DirectML for the earlier candidate (E35). The cause is the fp16 tail, not the missing epsilon in the per-layer norms:

- Adding an epsilon to all 146 norms (an add form and a max form) still returned NaN. Both forms add two nodes per norm (+292); the max-form graph as built had +584 because the fp16 converter kept each `Max` in fp32 between two Casts.
- Zero rows inside a normal sequence, masked or attended, were already finite.
- The final L2 normalisation used a floor of 1e-12, which is not representable in fp16 and flushes to zero on the GPU, giving 0/0 for a zero pooled vector. The CPU provider does not reproduce this.

Fix in E40: cast the pooled, projected vector to fp32 first, then `ReduceL2 -> Max(n, 1e-12) -> Div` in fp32; pooling count floor is `Max(count, 1.0)`. On DirectML an all-zero input and an all-zero attention mask both return zeros. `scripts\groupE\zero_check.py --provider dml` checks both cases (all-zero input with an all-ones mask; normal input with an all-zero mask) and fails on a non-finite output; on the rebuilt E40 it passes at lengths 32 and 128 on DirectML device 0. The build's own check (recorded in `manifest.json`) uses the CPU provider, which returns zeros even for the unfixed graph, so only the DirectML mode detects this fault. The original diagnosis runs on the earlier candidates were done by hand before that mode existed. Four nodes fewer than E35, same speed and parity.

## Variant names

The scripts can build many variants; the letters are the export groups of the comparison. A: contrib operators replaced by standard ones, FP32. B: half precision. C: quantised (not included here). D: static per-length graphs with the LpNormalization norm. E: fewest nodes (single-node Gelu, norm scale folded into weights, four-node RoPE); E37 and E38 are the per-norm epsilon experiments. Variant ids such as B15 or D29 appear in code comments and manifest notes for that reason (the token table is derived from the source graph's embedding initializer). Only E40 is the supported output; the others were not rebuilt from this folder.

Generated nodes are named `alc/<...>`; the prefix is only a namespace for nodes the passes add, and it is part of the reproduced bytes.

## Contract

- Inputs: `inputs_embeds` float16 `[1, L, 512]` = `embed_tokens.f16.npy[token_id]`, unscaled, looked up on the host; `attention_mask` int64 `[1, L]` (1 on real tokens, 0 on padding).
- Outputs: `sentence_embedding` float32 `[1, 768]` (mean pooling over the mask, projection and L2 normalisation all in graph); `last_hidden_state` float32 `[1, L, 768]`.
- Prompt prefixes applied by the caller: `task: search result | query: ` and `title: none | text: `.
- Output files: `model_32.onnx` ... `model_512.onnx`, `model.onnx_data` (290,203,648 B), `embed_tokens.f16.npy` (268,435,584 B).

## Measured

DirectML device 0 = RTX 4070 Laptop 8 GB, pip onnxruntime-directml 1.24.4 (Python), batch 1, padded fixed buckets. Reference = unmodified FP32 graph on the ONNX Runtime CPU provider. Public corpus = CPython 3.11.0 standard library source chunks (4,000 documents + 200 queries). CPU load measured just before the run: 76 % (other work on the machine). p50 is the comparison number; p95 was taken under load and is not a clean number.

| Tokens | p50 ms | p95 ms (under load) | Texts in bucket |
|---|---|---|---|
| 32 | 6.4 | 6.7 | 352 |
| 64 | 7.5 | 8.0 | 814 |
| 128 | 10.6 | 11.3 | 726 |
| 256 | 15.6 | 16.7 | 693 |
| 512 | 24.8 | 26.6 | 1,615 |

Parity on the public corpus: min cosine 0.99966, 1st percentile 0.99990, mean 0.99998; top-10 overlap 0.990; top-1 agreement 0.985; 59 texts/s including tokenising.

A private corpus of the same size (code and documentation, not distributed) gave the same picture: p50 6.4 / 7.6 / 10.8 / 16.0 / 25.7 ms, min cosine 0.99989, top-10 overlap 0.989, top-1 agreement 1.0, at 45 % CPU load.

Single passage, 300 iterations per length (CPU load 45 %): p50 5.8 / 6.9 / 10.0 / 15.4 / 27.0 ms; load 1.7 to 3.0 s per length; first inference 13 to 81 ms; process peak working set 761 MB; GPU memory +477 MiB with all five lengths loaded in sequence.

Intel Iris Xe (DirectML device 1), earlier candidate E35: correct (min cosine 0.99998) but 79 / 94 / 148 / 233 / 472 ms, about the speed of FP32 on the CPU. No gain; integrated graphics should take the CPU path.

## Known limits

- Rust `ort` =2.0.0-rc.13 with DirectML (onnxruntime 1.24.4 DLL, private corpus): p50 6.6 / 7.7 / 10.9 / 15.9 / 25.4 ms, min cosine 0.99989, top-10 overlap 0.989. Not run through rightkit-embed. TensorRT not run.
- Only E40 was rebuilt from this cleaned folder; the other variants the scripts can build were not re-verified here.
- Gelu erf form instead of tanh form; covered by the parity figures above.
- All-zero input and all-zero mask checked on DirectML at lengths 32 and 128 only.
- The fixed passage in `bench.py` was written for this benchmark; it is not taken from any other document.
- `ep.dml.enable_graph_capture` gave implausible timings and different values; not used.
- Quantised variants (q4, dynamic q8) fail parity on every provider tested (cosine 0.93 to 0.985); they are not part of this export.
