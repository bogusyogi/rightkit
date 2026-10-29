# Sherpa-ONNX keyword-spotting reference fixtures

Reference outputs from the unpatched sherpa-onnx 1.13.4 PyPI wheel (bundled onnxruntime 1.27.0, kaldi-native-fbank 1.22.3, Linux x86_64) for the `rightkit-asr` KWS parity tests. The model is `sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20`, using the chunk-16 int8 encoder, the fp32 decoder and the int8 joiner. `reference.json` records the SHA-256 of each model file.

| Path | Contents |
| --- | --- |
| `reference.json` | Schema `rightkit.kws-reference.v1`: keyword sets, cases with their decode counts and detections (keyword, tokens, timestamps as returned), and the fbank and encoder file index |
| `fbank/*.f32le` | Sherpa's own fbank frames, raw little-endian f32, frames x 80. Each case's `fbank` file holds the frames for its whole fed audio (segments + tail). Not committed (7 MB, git-ignored): regenerate them (below); `reference.json` holds each file's SHA-256 and the tests check it |
| `encoder/en_0.f32le` | int8 encoder output from Python onnxruntime 1.27.0 for the first 8 chunks of `en_0.wav`, 8 x 320 per chunk, fed from `fbank/en_0.f32le` |
| `audio/*.wav` | Three Sherpa test clips (see Attribution) |
| `generate_reference.py` | The generator; its docstring states the feeding protocol |

Cases that use `short_9s.wav`, `long_38s.wav` and `filler_retake.wav` read them from the parent `tests/fixtures/` directory.

## Regenerate

The exact-parity test (`kws_detections_match_sherpa_reference`) and the fbank test need the fbank dumps; without them they skip (and fail under `RIGHTKIT_REQUIRE_ASSETS=1`). Python 3.10+ with `pip install sherpa-onnx==1.13.4 kaldi-native-fbank==1.22.3 onnxruntime==1.27.0 numpy soundfile`, and the upstream model archive extracted:

```sh
python3 -I generate_reference.py \
  --model-dir    <dir with the encoder, decoder, joiner, tokens.txt and keywords-*.txt> \
  --upstream-dir <extracted sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20>
```

Each run writes the same bytes.

## Attribution

`audio/en_0.wav`, `audio/en_1.wav` and `audio/zh_3.wav` are copied unmodified from `test_wavs/` in the `sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20` model archive (SHA-256 `68447f4fbc67e70eee3a93961f36e81e98f47aef73ce7e7ca00885c6cd3616a6`). The k2-fsa sherpa-onnx project distributes that archive under the Apache License 2.0 (<https://github.com/k2-fsa/sherpa-onnx>, <http://www.apache.org/licenses/LICENSE-2.0>). The `upstream_test` keyword set is the archive's `test_wavs/keywords.txt`, used verbatim.

Unverified: the transcripts of the two English clips match LibriSpeech test-clean utterances, and LibriSpeech is released under CC BY 4.0.
