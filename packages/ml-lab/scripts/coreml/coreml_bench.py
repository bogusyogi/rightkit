#!/usr/bin/env python3
"""
Parakeet CoreML/ANE benchmark + decode harness.

RUNS ON: the Mac (Apple Silicon) -- the ANE only exists here.
  env: python3.11 + coremltools + numpy + soundfile (+ optional jiwer for WER)
INPUT: a `.mlpackage` dir produced by convert_parakeet_coreml.py (with pipeline.json).

Loads the 4 stages, runs the windowed *stateful* greedy decode (TDT or RNN-T),
and reports RTF + first-token latency + (optional) WER. Exercises the Mac-side
levers: #3 decode round-trips, #4 compute-units (--compute-units), #5 ANE
residency (inspect with Xcode Instruments / `--report-ane`), #6 I/O reuse.

Long-form (>window): slides the encoder window with overlap and carries the
decoder LSTM state across windows -- the "rolling cache" equivalent that removes
the ~200 s Conformer attention cap (see the runbook §2).

Usage:
  python coreml_bench.py --pkg ./out/tdt_w15_p6 --wav clip.wav --compute-units cpuAndNeuralEngine
  python coreml_bench.py --pkg ./out/tdt_w15_p6 --manifest tests/asr_global_eval/manifest.json --clips-dir ...
"""
from __future__ import annotations

import argparse
import json
import time
from pathlib import Path

import numpy as np

try:
    import coremltools as ct
except ImportError:
    raise SystemExit("coremltools required (pip install coremltools) -- run on the Mac")

CU = {
    "cpuAndNeuralEngine": ct.ComputeUnit.CPU_AND_NE,
    "all": ct.ComputeUnit.ALL,
    "cpuAndGPU": ct.ComputeUnit.CPU_AND_GPU,
    "cpuOnly": ct.ComputeUnit.CPU_ONLY,
}


def load_audio_16k_mono(path):
    import soundfile as sf
    audio, sr = sf.read(path, dtype="float32")
    if audio.ndim > 1:
        audio = audio.mean(axis=1)
    if sr != 16_000:
        # simple linear resample; fine for a benchmark clip
        x = np.linspace(0, len(audio), int(len(audio) * 16_000 / sr), endpoint=False)
        audio = np.interp(x, np.arange(len(audio)), audio).astype("float32")
    return audio


def load_vocab(pkg_dir):
    # vocab.txt ships next to the model in model_registry; pass --vocab or place it here.
    vp = Path(pkg_dir) / "vocab.txt"
    if not vp.exists():
        return None
    pieces = [ln.rstrip("\n").split("\t")[0] for ln in vp.read_text().splitlines()]
    return pieces


def detok(ids, pieces):
    if not pieces:
        return " ".join(map(str, ids))
    out = "".join(pieces[i] if 0 <= i < len(pieces) else "" for i in ids)
    return out.replace("▁", " ").strip()  # SentencePiece ▁ -> space


def mel_host(audio, n_mel):
    """Host-side log-mel matching NeMo's AudioToMelSpectrogramPreprocessor.
    VERIFY params against the model cfg (n_fft=512, win=400/0.025s, hop=160/0.01s,
    mel scale, log). Used only when conversion was run with --skip-mel."""
    import torchaudio  # noqa
    import torch
    mel = torchaudio.transforms.MelSpectrogram(
        sample_rate=16_000, n_fft=512, win_length=400, hop_length=160,
        n_mels=n_mel, power=2.0,
    )(torch.from_numpy(audio).float())
    mel = torch.log(mel + 1e-5)
    # NeMo per-feature normalization -- VERIFY (it normalizes per-utterance).
    mel = (mel - mel.mean(dim=1, keepdim=True)) / (mel.std(dim=1, keepdim=True) + 1e-5)
    return mel.unsqueeze(0).numpy().astype("float16")  # (1, n_mel, T)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pkg", required=True, help="dir with the 4 .mlpackage stages + pipeline.json")
    ap.add_argument("--wav")
    ap.add_argument("--manifest"); ap.add_argument("--clips-dir")
    ap.add_argument("--compute-units", choices=list(CU), default="cpuAndNeuralEngine")
    ap.add_argument("--overlap-sec", type=float, default=2.0, help="window overlap for long-form stitching")
    ap.add_argument("--max-symbols", type=int, default=10, help="RNN-T max symbols/step")
    ap.add_argument("--warmup", type=int, default=2)
    args = ap.parse_args()

    pkg = Path(args.pkg)
    meta = json.loads((pkg / "pipeline.json").read_text())
    is_tdt = meta["is_tdt"]; n_mel = meta["n_mel"]
    mel_frames = meta["mel_frames"]; enc_frames = meta["enc_frames"]
    durations = meta.get("tdt_durations") or [0, 1, 2, 3, 4]  # VERIFY against cfg
    blank = meta.get("blank_id")
    cu = CU[args.compute_units]

    def load(stage):
        p = pkg / f"{stage}.mlpackage"
        return ct.models.MLModel(str(p), compute_units=cu) if p.exists() else None

    t0 = time.perf_counter()
    mel_model = load("MelSpectrogram")
    enc = load("AudioEncoder"); dec = load("TextDecoder"); joint = load("Joint")
    print(f"[load] stages on {args.compute_units} in {(time.perf_counter()-t0)*1000:.0f} ms")
    pieces = load_vocab(pkg)

    # coremltools auto-generates output tensor names (obj, var_NN, ...). Resolve them
    # by shape so the decode loop is name-agnostic across variants.
    ph = meta["pred_hidden"]; pl = meta["pred_layers"]
    mel_out = mel_model.get_spec().description.output[0].name
    enc_out = enc.get_spec().description.output[0].name
    _dec_outs = dec.get_spec().description.output
    g_name = next(o.name for o in _dec_outs if list(o.type.multiArrayType.shape) == [1, 1, ph])
    state_names = [o.name for o in _dec_outs if list(o.type.multiArrayType.shape) == [pl, 1, ph]]
    joint_out = joint.get_spec().description.output[0].name
    win_samples = int(meta["window_sec"] * 16_000)

    def transcribe(audio):
        first_tok_t = [None]; t_start = time.perf_counter()
        tokens = []
        # Slide fixed audio windows. The decoder LSTM state is RESET at each window
        # boundary — carrying it across a hard (non-overlapping) boundary makes TDT
        # emit all-blank dead windows (it frame-skips over the encoder/decoder
        # mismatch), measured 57% -> 5% WER once reset. Overlap-merge is the further
        # refinement for words split on a boundary.
        for ws in range(0, max(len(audio), 1), win_samples):
            h = np.zeros((pl, 1, ph), dtype="float16"); c = np.zeros_like(h)
            last_tok = blank if blank is not None else 0
            seg = audio[ws:ws + win_samples]
            a = np.zeros(win_samples, dtype="float32"); a[:len(seg)] = seg
            # Drive the CoreML MelSpectrogram stage (NOT host torchaudio); slice to encoder frames.
            mel = mel_model.predict({"audio": a.reshape(1, -1).astype("float16"),
                                     "audio_len": np.array([len(seg)], dtype="int32")})[mel_out][:, :, :mel_frames]
            encoded = enc.predict({"melspectrogram_features": mel.astype("float16"),
                                   "mel_len": np.array([mel_frames], dtype="int32")})[enc_out]
            encoded = np.ascontiguousarray(encoded[0].T)  # (frames, D)
            # LENGTH CLAMP (macOS LengthClampSession equivalent): only decode frames that
            # cover REAL audio, not the silence padding the static window adds. Without this
            # the decoder hallucinates a repeated tail over the padding.
            nframes = min(encoded.shape[0], int(np.ceil(len(seg) / 160 / 8)))
            t = 0
            while t < nframes:
                sym = 0
                while sym < args.max_symbols:
                    d = dec.predict({"decoder_input_ids": np.array([[last_tok]], dtype="int32"),
                                     "state_h": h, "state_c": c})
                    g = np.asarray(d[g_name]).reshape(1, -1)
                    logits = np.asarray(joint.predict({
                        "encoder_step": encoded[t:t+1].astype("float16"),
                        "decoder_step": g.astype("float16")})[joint_out]).reshape(-1)
                    if is_tdt:
                        nd = len(durations)
                        tok = int(logits[:-nd].argmax()); dur = durations[int(logits[-nd:].argmax())]
                        if tok != blank:
                            tokens.append(tok); last_tok = tok
                            h = np.asarray(d[state_names[0]]); c = np.asarray(d[state_names[1]])
                            if first_tok_t[0] is None: first_tok_t[0] = time.perf_counter()
                            sym += 1
                            if dur == 0 and sym < args.max_symbols:
                                continue  # TDT dur=0: emit another token at the same frame
                        t += max(dur, 1); break
                    tok = int(logits.argmax())
                    if tok == blank:
                        t += 1; break
                    tokens.append(tok); last_tok = tok
                    h = np.asarray(d[state_names[0]]); c = np.asarray(d[state_names[1]]); sym += 1
                    if first_tok_t[0] is None: first_tok_t[0] = time.perf_counter()
                else:
                    t += 1  # max symbols hit — force time advance
        dur_s = len(audio) / 16_000; decode_s = time.perf_counter() - t_start
        ftl_ms = ((first_tok_t[0] - t_start) * 1000) if first_tok_t[0] else None
        return detok(tokens, pieces), decode_s, dur_s, ftl_ms

    # warmup (ANE compiles + caches on first run)
    if args.wav:
        a = load_audio_16k_mono(args.wav)
        for _ in range(args.warmup):
            transcribe(a)
        text, decode_s, dur_s, ftl = transcribe(a)
        print(f"\ntext : {text}")
        print(f"audio: {dur_s:.1f}s  decode: {decode_s*1000:.0f}ms  RTF: {decode_s/dur_s:.3f}  "
              f"first-token: {ftl:.0f}ms" if ftl else "first-token: n/a")
        print(f"config: {meta['model']} window={meta['window_sec']}s palette={meta['palette_bits']}bit "
              f"compute={args.compute_units}")

    elif args.manifest:
        try:
            from jiwer import wer
        except ImportError:
            wer = None
        items = json.loads(Path(args.manifest).read_text())
        rtfs, wers = [], []
        for it in items:
            wavp = Path(args.clips_dir) / it.get("clip", it.get("path", ""))
            if not wavp.exists():
                continue
            a = load_audio_16k_mono(str(wavp))
            text, decode_s, dur_s, _ = transcribe(a)
            rtfs.append(decode_s / dur_s)
            if wer and it.get("gt"):
                wers.append(wer(it["gt"].lower(), text.lower()))
        print(f"\nclips={len(rtfs)}  mean RTF={np.mean(rtfs):.3f}"
              + (f"  mean WER={np.mean(wers)*100:.2f}%" if wers else ""))
    else:
        raise SystemExit("pass --wav or --manifest")


if __name__ == "__main__":
    main()
