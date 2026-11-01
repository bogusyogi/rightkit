#!/usr/bin/env python3
"""
Ground-truth WER eval over the pico_eval bundle (100 clips: 50 librispeech +
50 gigaspeech, 16 kHz mono, ~15 min). Runs on the Mac (ANE). Mirrors the exact
match protocol: per-clip transcribe -> identical norm() on hyp+GT -> jiwer.wer
overall + per-corpus.

Reuses the canonical windowed state-reset decode from coreml_bench.py (decoder
LSTM state RESET at each window boundary; TDT dur=0 repeat; length clamp).

Usage:
  ~/.hr-coreml-venv/bin/python pico_eval.py \
      --eval-dir /Volumes/Extra/pico_eval \
      --pkg ~/hr-coreml-models/out/tdt_w15_p6 [--pkg ...]
"""
from __future__ import annotations
import argparse, json, os, re, subprocess, time
from pathlib import Path
import numpy as np
import coremltools as ct
from jiwer import wer

CU = ct.ComputeUnit.CPU_AND_NE


def dir_size_mb(p):
    total = 0
    for root, _, files in os.walk(p):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(root, f))
            except OSError:
                pass
    return total / (1024 * 1024)


def self_rss_mb():
    # macOS `ps` RSS is in KB; current resident set of this process (incl. the
    # resident model + python/coremltools overhead). No VRAM on UMA — this is the
    # unified-memory footprint. Sample at steady state (post-load + warmup).
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(os.getpid())],
                         capture_output=True, text=True).stdout.strip()
    return (int(out) / 1024) if out else 0.0


def norm(t: str) -> str:
    t = t.lower()
    t = re.sub(r"<[^>]+>", " ", t)      # strip gigaspeech <COMMA>/<QUESTIONMARK>
    t = re.sub(r"[^a-z0-9 ]", " ", t)   # drop punctuation, keep alnum + space
    return re.sub(r"\s+", " ", t).strip()


def load_audio_16k_mono(path):
    import soundfile as sf
    audio, sr = sf.read(path, dtype="float32")
    if audio.ndim > 1:
        audio = audio.mean(axis=1)
    if sr != 16_000:
        x = np.linspace(0, len(audio), int(len(audio) * 16_000 / sr), endpoint=False)
        audio = np.interp(x, np.arange(len(audio)), audio).astype("float32")
    return audio


def load_vocab(pkg_dir):
    vp = Path(pkg_dir) / "vocab.txt"
    pieces = [ln.rstrip("\n").split("\t")[0] for ln in vp.read_text().splitlines()]
    return pieces


def encode_phrase_greedy(phrase, pieces):
    """Greedy longest-piece tokenisation of a bias phrase, mirroring the engine's
    `encode_phrase_greedy`. Leading word boundary is the sentencepiece marker."""
    text = "\u2581" + phrase.strip().replace(" ", "\u2581")
    index = {pc: i for i, pc in enumerate(pieces)}
    ids, i = [], 0
    while i < len(text):
        for j in range(len(text), i, -1):
            if text[i:j] in index:
                ids.append(index[text[i:j]]); i = j; break
        else:
            return None          # unrepresentable phrase -> engine drops it
    return ids


def matching_prefix_len(emitted, phrase):
    """Longest suffix of `emitted` that is a proper prefix of `phrase`."""
    mx = min(len(emitted), len(phrase) - 1)
    for ln in range(mx, -1, -1):
        if ln == 0 or list(emitted[len(emitted) - ln:]) == list(phrase[:ln]):
            return ln
    return 0


class ContextBias:
    """Additive logit bonus on the token that CONTINUES an in-progress phrase."""

    def __init__(self, phrases, score, pieces):
        self.score = score
        self.phrases = [e for e in (encode_phrase_greedy(p, pieces) for p in phrases) if e]

    def bonuses(self, emitted):
        out = {}
        for ph in self.phrases:
            m = matching_prefix_len(emitted, ph)
            if m < len(ph):
                out[ph[m]] = max(out.get(ph[m], 0.0), self.score)
        return out

    def argmax(self, logits, emitted):
        b = self.bonuses(emitted)
        if not b:
            return int(logits.argmax())
        best_i, best_v = 0, float("-inf")
        for i, l in enumerate(logits):
            v = float(l) + b.get(i, 0.0)
            if v > best_v:
                best_v, best_i = v, i
        return best_i


def detok(ids, pieces):
    out = "".join(pieces[i] if 0 <= i < len(pieces) else "" for i in ids)
    return out.replace("▁", " ").strip()


class Model:
    def __init__(self, pkg):
        self.pkg = Path(pkg)
        self.meta = json.loads((self.pkg / "pipeline.json").read_text())
        self.is_tdt = self.meta["is_tdt"]
        self.mel_frames = self.meta["mel_frames"]
        self.durations = self.meta.get("tdt_durations") or [0, 1, 2, 3, 4]
        self.blank = self.meta.get("blank_id")
        self.ph = self.meta["pred_hidden"]; self.pl = self.meta["pred_layers"]
        self.win_samples = int(self.meta["window_sec"] * 16_000)
        def load(s):
            # Prefer the source .mlpackage; fall back to the COMPILED .mlmodelc so
            # the eval can run the exact artifact the installed app ships (the
            # app bundles only compiled models — there is no .mlpackage on disk).
            pkgp = self.pkg / f"{s}.mlpackage"
            if pkgp.exists():
                return ct.models.MLModel(str(pkgp), compute_units=CU)
            return ct.models.CompiledMLModel(str(self.pkg / f"{s}.mlmodelc"), compute_units=CU)
        self.mel_model = load("MelSpectrogram"); self.enc = load("AudioEncoder")
        self.dec = load("TextDecoder"); self.joint = load("Joint")
        self.pieces = load_vocab(self.pkg)
        self.bias = None
        try:
            # Source models: read output names from the spec, states picked by shape.
            self.mel_out = self.mel_model.get_spec().description.output[0].name
            self.enc_out = self.enc.get_spec().description.output[0].name
            do = self.dec.get_spec().description.output
            self.g_name = next(o.name for o in do if list(o.type.multiArrayType.shape) == [1, 1, self.ph])
            self.state_names = [o.name for o in do if list(o.type.multiArrayType.shape) == [self.pl, 1, self.ph]]
            self.joint_out = self.joint.get_spec().description.output[0].name
        except Exception:
            # Compiled models have no spec — discover outputs with one dummy
            # predict and apply the SAME shape rules to the returned arrays.
            probe = self.dec.predict({
                "decoder_input_ids": np.zeros((1, 1), dtype="int32"),
                "state_h": np.zeros((self.pl, 1, self.ph), dtype="float16"),
                "state_c": np.zeros((self.pl, 1, self.ph), dtype="float16"),
            })
            self.g_name = next(k for k, v in probe.items() if np.asarray(v).shape == (1, 1, self.ph))
            states = [k for k, v in probe.items() if np.asarray(v).shape == (self.pl, 1, self.ph)]
            # Spec order was (h, c); recover it from the names, not dict order.
            self.state_names = sorted(states, key=lambda n: 0 if n.rstrip("_0123456789").endswith("h") else 1)
            self.mel_out = None  # single-output models: take the sole value at predict time
            self.enc_out = None
            self.joint_out = None

    # --- padded_window: the SHIPPED long-audio policy -----------------------
    # asr/runtime.rs::transcribe_padded_window. The naive fixed-stride
    # chop this replaces cuts mid-word every 15 s, which is what made every long
    # clip look bad regardless of model. Constants are the engine's.
    PADDING_MS, SILENCE_MS, SILENCE_HOP_MS, OVERLAP_CHARS = 2240, 200, 100, 16

    @staticmethod
    def _rms(x):
        return float(np.sqrt(np.mean(np.square(x)))) if len(x) else 0.0

    def _quiet_cut_in(self, audio, lo, hi):
        win = self.SILENCE_MS * 16
        hop = self.SILENCE_HOP_MS * 16
        if hi <= lo or (hi - lo) < win:
            return None
        best = None
        i = lo
        while i + win <= hi:
            v = self._rms(audio[i:i + win])
            if best is None or v < best[1]:
                best = (i + win // 2, v)
            i += hop
        return best[0] if best else None

    @staticmethod
    def _append_with_overlap(buf, nxt):
        if not nxt:
            return buf
        mx = min(len(buf), len(nxt), Model.OVERLAP_CHARS)
        best = 0
        for ln in range(1, mx + 1):
            if buf[-ln:].lower() == nxt[:ln].lower():
                best = ln
        return buf + nxt[best:]

    def transcribe(self, audio, max_symbols=10):
        """Long audio via the engine's padded_window policy; short audio decodes whole."""
        if len(audio) <= self.win_samples:
            return self._decode_window(audio, max_symbols)[0]
        padding = self.PADDING_MS * 16
        target = max(self.win_samples - padding, 1)
        text, start = "", 0
        while start < len(audio):
            win_end = min(start + self.win_samples, len(audio))
            tgt_end = min(start + target, len(audio))
            if win_end < len(audio):
                stable_end = self._quiet_cut_in(audio, tgt_end, win_end) or win_end
            else:
                stable_end = len(audio)
            _, timed = self._decode_window(audio[start:win_end], max_symbols)
            # Commit only tokens that START before the quiet cut, then resume there.
            limit = (stable_end - start) / 16000.0
            piece = "".join(pc for pc, ts in timed if ts < limit)
            text = self._append_with_overlap(text, piece.replace("\u2581", " "))
            start = max(stable_end, start + 1)
        return text.strip()

    def _decode_window(self, audio, max_symbols=10):
        tokens, timed = [], []
        for ws in range(0, max(len(audio), 1), self.win_samples):
            h = np.zeros((self.pl, 1, self.ph), dtype="float16"); c = np.zeros_like(h)
            last_tok = self.blank if self.blank is not None else 0
            seg = audio[ws:ws + self.win_samples]
            a = np.zeros(self.win_samples, dtype="float32"); a[:len(seg)] = seg
            mel_d = self.mel_model.predict({"audio": a.reshape(1, -1).astype("float16"),
                                            "audio_len": np.array([len(seg)], dtype="int32")})
            mel = (mel_d[self.mel_out] if self.mel_out else next(iter(mel_d.values())))[:, :, :self.mel_frames]
            enc_d = self.enc.predict({"melspectrogram_features": mel.astype("float16"),
                                      "mel_len": np.array([self.mel_frames], dtype="int32")})
            encoded = enc_d[self.enc_out] if self.enc_out else next(iter(enc_d.values()))
            encoded = np.ascontiguousarray(encoded[0].T)
            nframes = min(encoded.shape[0], int(np.ceil(len(seg) / 160 / 8)))
            t = 0
            while t < nframes:
                sym = 0
                while sym < max_symbols:
                    d = self.dec.predict({"decoder_input_ids": np.array([[last_tok]], dtype="int32"),
                                          "state_h": h, "state_c": c})
                    g = np.asarray(d[self.g_name]).reshape(1, -1)
                    j_d = self.joint.predict({
                        "encoder_step": encoded[t:t+1].astype("float16"),
                        "decoder_step": g.astype("float16")})
                    logits = np.asarray(j_d[self.joint_out] if self.joint_out else next(iter(j_d.values()))).reshape(-1)
                    if self.is_tdt:
                        nd = len(self.durations)
                        tok = (self.bias.argmax(logits[:-nd], tokens) if self.bias else int(logits[:-nd].argmax())); dur = self.durations[int(logits[-nd:].argmax())]
                        if tok != self.blank:
                            tokens.append(tok); last_tok = tok
                            timed.append((self.pieces[tok] if tok < len(self.pieces) else '', (ws + t * 160 * 8) / 16000.0))
                            h = np.asarray(d[self.state_names[0]]); c = np.asarray(d[self.state_names[1]])
                            sym += 1
                            if dur == 0 and sym < max_symbols:
                                continue
                        t += max(dur, 1); break
                    tok = self.bias.argmax(logits, tokens) if self.bias else int(logits.argmax())
                    if tok == self.blank:
                        t += 1; break
                    tokens.append(tok); last_tok = tok
                    timed.append((self.pieces[tok] if tok < len(self.pieces) else '', (ws + t * 160 * 8) / 16000.0))
                    h = np.asarray(d[self.state_names[0]]); c = np.asarray(d[self.state_names[1]]); sym += 1
                else:
                    t += 1
        return detok(tokens, self.pieces), timed


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--eval-dir", required=True)
    ap.add_argument("--pkg", action="append", required=True)
    ap.add_argument("--out-json")
    # Context biasing: the shipped engine enables this by DEFAULT at score 1.0
    # (asr/models.rs::context_bias_score). An eval without it measures
    # an unbiased floor the product never actually ships.
    ap.add_argument("--bias-terms", help="file with one bias phrase per line")
    ap.add_argument("--bias-score", type=float, default=1.0)
    args = ap.parse_args()

    ev = Path(args.eval_dir)
    items = json.loads((ev / "eval_set.json").read_text())
    # cache audio once (shared across variants)
    audio_cache = {it["wav"]: load_audio_16k_mono(str(ev / it["wav"])) for it in items}
    total_audio_s = sum(it["dur_s"] for it in items)

    results = {}
    hdr = (f"{'variant':<18} {'overall':>8} {'libri':>8} {'giga':>8} "
           f"{'RTF':>7} {'decode_s':>9} {'disk_MB':>8} {'RSS_MB':>8}")
    print(hdr); print("-" * len(hdr))
    for pkg in args.pkg:
        name = Path(pkg).name
        disk_mb = dir_size_mb(pkg)
        m = Model(pkg)
        if args.bias_terms:
            terms = [l.strip() for l in open(args.bias_terms) if l.strip()]
            m.bias = ContextBias(terms, args.bias_score, m.pieces)
            print(f"  context bias: {len(m.bias.phrases)}/{len(terms)} phrases installed @ score {args.bias_score}")
        refs, hyps, corp, raw_hyps = [], [], [], []
        # warmup (also brings the model fully resident before the RSS sample)
        m.transcribe(audio_cache[items[0]["wav"]])
        rss_mb = self_rss_mb()
        t0 = time.perf_counter()
        for it in items:
            hyp = m.transcribe(audio_cache[it["wav"]])
            # Keep the RAW hypothesis alongside the normalised one. Storing only
            # the normalised text silently strips punctuation and casing, which
            # makes any later comparison against another engine's raw output a
            # false formatting gap (this cost a full review round on 2026-07-18).
            raw_hyps.append(hyp)
            refs.append(norm(it["gt"])); hyps.append(norm(hyp)); corp.append(it["corpus"])
        decode_s = time.perf_counter() - t0
        rss_mb = max(rss_mb, self_rss_mb())  # peak across the run
        ov = wer(refs, hyps)
        per = {}
        for cname in sorted(set(corp)):
            r = [refs[i] for i in range(len(refs)) if corp[i] == cname]
            hy = [hyps[i] for i in range(len(hyps)) if corp[i] == cname]
            per[cname] = wer(r, hy)
        rtf = decode_s / total_audio_s
        results[name] = {"overall": ov, "per_corpus": per, "rtf": rtf, "decode_s": decode_s,
                         "disk_mb": round(disk_mb, 1), "rss_mb": round(rss_mb, 1),
                         "hyps": {items[i]["wav"]: hyps[i] for i in range(len(items))},
                         "raw_hyps": {items[i]["wav"]: raw_hyps[i] for i in range(len(items))}}
        print(f"{name:<18} {ov*100:>7.2f}% {per.get('librispeech',0)*100:>7.2f}% "
              f"{per.get('gigaspeech',0)*100:>7.2f}% {rtf:>7.4f} {decode_s:>8.1f}s "
              f"{disk_mb:>8.0f} {rss_mb:>8.0f}")
        del m  # free before next variant so the RSS sample reflects one model

    if args.out_json:
        Path(args.out_json).write_text(json.dumps(results, indent=2))
        print(f"\nwrote {args.out_json}")


if __name__ == "__main__":
    main()
