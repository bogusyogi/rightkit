#!/usr/bin/env python3
"""Regenerate the Sherpa-ONNX keyword-spotting reference fixtures (rightkit.kws-reference.v1).

The reference is the unpatched sherpa-onnx PyPI wheel. It runs every case below through the low-level
KeywordSpotter binding and writes reference.json, raw fbank frames (fbank/*.f32le) and raw encoder
outputs from Python onnxruntime (encoder/*.f32le). The rightkit-asr Rust parity tests consume them.

Requirements: sherpa_onnx, kaldi-native-fbank, onnxruntime, numpy, soundfile. Run with `python3 -I`:

    python3 -I generate_reference.py \\
        --model-dir   <dir with the zh-en-3M chunk-16 encoder/decoder/joiner, tokens.txt, keywords-*.txt> \\
        --upstream-dir <extracted sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20>

Feeding protocol for every case (the Rust side replicates it exactly): audio = the concatenated
`segments` (a wav name, or {"silence_samples": n} zeros) followed by `tail_silence_samples` zeros, fed in
consecutive chunks of `feed_samples`. After each accept_waveform: while is_ready, decode once, then
get_result; a non-empty keyword is recorded with the 0-based index of that decode. input_finished is
never called. Each case also names a fbank file: Sherpa's own frames for its whole fed audio (segments +
tail), read with get_frames from an undecoded stream that accepted that audio in one call.
"""

import argparse
import ctypes
import hashlib
import importlib.metadata
import json
import platform
import shutil
import sys
import tempfile
from pathlib import Path

import numpy as np
import soundfile as sf

SCHEMA = "rightkit.kws-reference.v1"
SAMPLE_RATE = 16000
ENCODER = "encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx"
DECODER = "decoder-epoch-13-avg-2-chunk-16-left-64.onnx"
JOINER = "joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx"
TOKENS = "tokens.txt"
# Sherpa test wavs copied into kws/audio/. zh_3 rather than zh_0: zh_0 holds none of the test keywords,
# zh_3 holds two, and its first detection sits on two near-tied merged beam paths.
COPIED_WAVS = ["en_0.wav", "en_1.wav", "zh_3.wav"]
RIGHTKIT_WAVS = ["short_9s.wav", "long_38s.wav", "filler_retake.wav"]  # already in tests/fixtures/
FBANK_AUDIO = ["en_0.wav", "short_9s.wav"]
FBANK_BUDGET_BYTES = 8_000_000  # past this, long_38s cases go without a fbank file
ENCODER_AUDIO = "en_0.wav"
ENCODER_MAX_CHUNKS = 8
TAIL = 3200

# Dense English set: common short words so detections are frequent on ordinary speech. It holds
# prefix pairs (IT/ITS, NO/NOTE, THING/THINK), siblings (THEN/THEM/THERE) and suffix words (AT in THAT,
# IS in HIS, INK in THINK) so Aho-Corasick fail and output arcs fire. HE has no boost and TO no
# threshold, so those two take the case-level keywords_score / keywords_threshold.
DENSE_WORDS = [
    ("THE", "DH AH0"), ("THEN", "DH EH1 N"), ("THEM", "DH EH1 M"), ("THERE", "DH EH1 R"),
    ("THIS", "DH IH1 S"), ("THAT", "DH AE1 T"), ("AT", "AE1 T"), ("AN", "AE1 N"), ("AS", "AE1 Z"),
    ("AND", "AH0 N D"), ("IN", "IH0 N"), ("IT", "IH1 T"), ("ITS", "IH1 T S"), ("IS", "IH1 Z"),
    ("HIS", "HH IH1 Z"), ("HIM", "HH IH1 M"), ("HE", "HH IY1"), ("HAD", "HH AE1 D"), ("TO", "T UW1"),
    ("YOU", "Y UW1"), ("OF", "AH1 V"), ("FOR", "F AO1 R"), ("WAS", "W AA1 Z"), ("WITH", "W IH1 DH"),
    ("ON", "AA1 N"), ("ARE", "AA1 R"), ("NOT", "N AA1 T"), ("NO", "N OW1"), ("NOTE", "N OW1 T"),
    ("BE", "B IY1"), ("BUT", "B AH1 T"), ("WHAT", "W AH1 T"), ("THING", "TH IH1 NG"),
    ("THINK", "TH IH1 NG K"), ("INK", "IH1 NG K"),
]
DENSE_BOOSTS = ["1.0", "1.5", "2.0", "2.5", "3.0"]
DENSE_THRESHOLDS = ["0.02", "0.05", "0.08", "0.1", "0.12", "0.15"]
DENSE_NO_BOOST = {"HE"}
DENSE_NO_THRESHOLD = {"TO"}


def dense_keywords_text():
    lines = []
    for i, (label, phones) in enumerate(DENSE_WORDS):
        parts = [phones]
        if label not in DENSE_NO_BOOST:
            parts.append(":" + DENSE_BOOSTS[i % len(DENSE_BOOSTS)])
        if label not in DENSE_NO_THRESHOLD:
            parts.append("#" + DENSE_THRESHOLDS[i % len(DENSE_THRESHOLDS)])
        parts.append("@" + label)
        lines.append(" ".join(parts))
    return "\n".join(lines) + "\n"


def build_cases():
    cases = []

    def add(case_id, segments, keyword_set, max_active_paths=4, num_trailing_blanks=1,
            keywords_score=1.0, keywords_threshold=0.25, feed_samples=320, tail=TAIL):
        cases.append({
            "id": case_id, "segments": segments, "keyword_set": keyword_set,
            "max_active_paths": max_active_paths, "num_trailing_blanks": num_trailing_blanks,
            "keywords_score": keywords_score, "keywords_threshold": keywords_threshold,
            "feed_samples": feed_samples, "tail_silence_samples": tail, "fbank": None,
        })

    for wav in COPIED_WAVS:
        stem = wav[:-4]
        for feed in [320, 1600, None]:  # None: the whole clip in one accept_waveform
            add(f"upstream_test/{stem}/feed_{feed or 'whole'}", [wav], "upstream_test", feed_samples=feed)

    for wav in RIGHTKIT_WAVS:
        stem = wav[:-4]
        for paths, blanks, feed in [(4, 1, 320), (8, 0, 800), (4, 0, 800), (8, 1, 320)]:
            add(f"dense/{stem}/p{paths}_b{blanks}_feed_{feed}", [wav], "dense",
                max_active_paths=paths, num_trailing_blanks=blanks, feed_samples=feed)

    for keyword_set in ["hr_wake", "hr_confirm", "hr_hey"]:
        for wav in RIGHTKIT_WAVS:
            add(f"{keyword_set}/{wav[:-4]}", [wav], keyword_set, max_active_paths=8, num_trailing_blanks=0)

    gap = {"silence_samples": 64000}
    reset_segments = ["en_0.wav", gap, "en_1.wav", gap, "en_0.wav"]
    add("reset/upstream_test/en_0_en_1_en_0", reset_segments, "upstream_test")
    add("reset/dense/en_0_en_1_en_0", reset_segments, "dense")
    add("reset/upstream_test/lead_silence", [gap, "en_0.wav", gap, "en_1.wav"], "upstream_test")
    add("reset/dense/lead_silence", [gap, "short_9s.wav"], "dense", feed_samples=800)

    add("dense/long_38s/p16_b1_feed_320", ["long_38s.wav"], "dense", max_active_paths=16)
    add("dense/short_9s/p16_b0_score2_thr005", ["short_9s.wav"], "dense", max_active_paths=16,
        num_trailing_blanks=0, keywords_score=2.0, keywords_threshold=0.05)
    add("upstream_test/en_1/score2_thr005", ["en_1.wav"], "upstream_test", keywords_score=2.0,
        keywords_threshold=0.05)
    add("upstream_test/en_0/feed_4799_tail_0", ["en_0.wav"], "upstream_test", feed_samples=4799, tail=0)
    add("upstream_test/en_1/feed_160_tail_10560", ["en_1.wav"], "upstream_test", feed_samples=160,
        tail=10560)
    add("hr_wake/long_38s/score2_thr005", ["long_38s.wav"], "hr_wake", max_active_paths=8,
        num_trailing_blanks=0, keywords_score=2.0, keywords_threshold=0.05)
    return cases


def sha256_file(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def load_wav(path):
    data, sr = sf.read(str(path), dtype="int16", always_2d=True)
    if sr != SAMPLE_RATE or data.shape[1] != 1:
        sys.exit(f"{path}: expected 16 kHz mono, got {sr} Hz x {data.shape[1]}")
    return data[:, 0].astype(np.float32) / np.float32(32768.0)


def case_audio(case, wavs):
    parts = []
    for seg in case["segments"]:
        parts.append(wavs[seg] if isinstance(seg, str) else np.zeros(seg["silence_samples"], np.float32))
    parts.append(np.zeros(case["tail_silence_samples"], np.float32))
    return np.concatenate(parts)


def check_tokens(name, text, tokens):
    for line in text.splitlines():
        for word in line.split():
            if word[0] not in ":#@" and word not in tokens:
                sys.exit(f"keyword set {name}: token {word!r} is not in {TOKENS}")


def wheel_onnxruntime_version(sherpa_onnx):
    """Version string of the onnxruntime library bundled in the sherpa_onnx wheel."""
    libdir = Path(sherpa_onnx.__file__).parent / "lib"
    libs = sorted(p for p in libdir.iterdir() if p.name.startswith(("libonnxruntime", "onnxruntime")))
    if not libs:
        return "unknown"

    class OrtApiBase(ctypes.Structure):
        _fields_ = [("GetApi", ctypes.c_void_p), ("GetVersionString", ctypes.CFUNCTYPE(ctypes.c_char_p))]

    lib = ctypes.CDLL(str(libs[0]))
    lib.OrtGetApiBase.restype = ctypes.POINTER(OrtApiBase)
    return lib.OrtGetApiBase().contents.GetVersionString().decode()


def run_case(low, case, audio):
    stream = low.create_stream()
    decodes = 0
    detections = []
    feed = case["feed_samples"]
    for start in range(0, len(audio), feed):
        stream.accept_waveform(SAMPLE_RATE, audio[start:start + feed])
        while low.is_ready(stream):
            low.decode_stream(stream)
            r = low.get_result(stream)
            if r.keyword != "":
                detections.append({"decode": decodes, "keyword": r.keyword, "tokens": list(r.tokens),
                                   "timestamps": [float(t) for t in r.timestamps]})
            decodes += 1
    return decodes, detections


def audio_key(case):
    """Name of a case's fed audio: wav names and silN joined by '+', then +tailN when there is a tail."""
    parts = [s if isinstance(s, str) else f"sil{s['silence_samples']}" for s in case["segments"]]
    if case["tail_silence_samples"]:
        parts.append(f"tail{case['tail_silence_samples']}")
    return "+".join(parts)


def sherpa_fbank(low, samples):
    """Sherpa's own fbank frames for `samples`, read from a stream that is never decoded."""
    stream = low.create_stream()
    stream.accept_waveform(SAMPLE_RATE, samples)
    n = len(samples)
    ready = 0 if n < 280 else (n - 280) // 160 + 1  # 25 ms / 10 ms frames, snip_edges=false, no flush
    return np.asarray(stream.get_frames(0, ready), dtype=np.float32).reshape(ready, 80)


def knf_fbank(samples):
    import kaldi_native_fbank as knf

    opts = knf.FbankOptions()
    opts.frame_opts.samp_freq = SAMPLE_RATE
    opts.frame_opts.dither = 0.0
    opts.frame_opts.snip_edges = False
    opts.frame_opts.remove_dc_offset = True
    opts.frame_opts.preemph_coeff = 0.97
    opts.frame_opts.window_type = "povey"
    opts.mel_opts.num_bins = 80
    opts.mel_opts.low_freq = 20.0
    opts.mel_opts.high_freq = -400.0
    fbank = knf.OnlineFbank(opts)
    fbank.accept_waveform(SAMPLE_RATE, samples.tolist())
    return np.array([fbank.get_frame(i) for i in range(fbank.num_frames_ready)], dtype=np.float32)


def encoder_outputs(encoder_path, frames, max_chunks):
    """int8 encoder outputs in Python onnxruntime, chunked exactly as Sherpa feeds the encoder."""
    import onnxruntime as ort

    opts = ort.SessionOptions()
    opts.intra_op_num_threads = 1
    opts.inter_op_num_threads = 1
    sess = ort.InferenceSession(str(encoder_path), opts, providers=["CPUExecutionProvider"])
    meta = sess.get_modelmeta().custom_metadata_map
    chunk, shift = int(meta["T"]), int(meta["decode_chunk_len"])

    states = {}
    for inp in sess.get_inputs():
        if inp.name == "x":
            continue
        shape = [d if isinstance(d, int) else 1 for d in inp.shape]
        dtype = np.int64 if inp.type == "tensor(int64)" else np.float32
        states[inp.name] = np.zeros(shape, dtype=dtype)
    out_names = [o.name for o in sess.get_outputs()]

    outs = []
    c = 0
    while c < max_chunks and c * shift + chunk <= len(frames):
        x = frames[c * shift:c * shift + chunk][None, :, :]
        results = sess.run(out_names, {"x": x, **states})
        for name, value in zip(out_names, results):
            if name.startswith("new_"):
                states[name[len("new_"):]] = value
        enc = results[out_names.index("encoder_out")]
        outs.append(enc[0])
        c += 1
    out = np.concatenate(outs, axis=0).astype(np.float32)
    return out, len(outs), outs[0].shape[0], outs[0].shape[1], ort.__version__


def make_spotter(sherpa_onnx, model_dir, keywords_file, paths, blanks, score, threshold):
    return sherpa_onnx.KeywordSpotter(
        tokens=str(model_dir / TOKENS), encoder=str(model_dir / ENCODER),
        decoder=str(model_dir / DECODER), joiner=str(model_dir / JOINER),
        keywords_file=str(keywords_file), num_threads=1, max_active_paths=paths,
        keywords_score=score, keywords_threshold=threshold, num_trailing_blanks=blanks, provider="cpu")


def render(value, indent=0, in_list=False):
    """JSON with one line per flat record in a list (detections, fbank entries); floats keep full repr."""
    pad = "  " * indent
    scalar = (str, int, float, bool, type(None))

    def flat(v):
        return isinstance(v, scalar) or (isinstance(v, list) and all(isinstance(x, scalar) for x in v))

    if isinstance(value, dict):
        one = json.dumps(value, ensure_ascii=False)
        if all(flat(v) for v in value.values()) and (in_list or len(one) <= 160):
            return one
        items = [f"{pad}  {json.dumps(k, ensure_ascii=False)}: {render(v, indent + 1)}" for k, v in value.items()]
        return "{\n" + ",\n".join(items) + "\n" + pad + "}"
    if isinstance(value, list):
        if not value:
            return "[]"
        if all(isinstance(x, scalar) for x in value) or len(json.dumps(value)) <= 120:
            return json.dumps(value, ensure_ascii=False)
        return "[\n" + ",\n".join(f"{pad}  {render(v, indent + 1, True)}" for v in value) + "\n" + pad + "]"
    return json.dumps(value, ensure_ascii=False)


def main():
    here = Path(__file__).resolve().parent
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--model-dir", type=Path, required=True)
    ap.add_argument("--upstream-dir", type=Path, required=True,
                    help="extracted sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20 archive")
    ap.add_argument("--out", type=Path, default=here, help="output dir (default: this script's dir)")
    ap.add_argument("--fixtures-dir", type=Path, default=here.parent,
                    help="rightkit-asr tests/fixtures (holds short_9s.wav etc.)")
    args = ap.parse_args()

    import sherpa_onnx

    model_dir, up, out = args.model_dir, args.upstream_dir, args.out
    for name in [ENCODER, DECODER, JOINER, TOKENS]:
        up_file = up / name
        if up_file.is_file() and sha256_file(up_file) != sha256_file(model_dir / name):
            print(f"warning: {name} differs from the upstream archive copy", file=sys.stderr)
    tokens = {line.split()[0] for line in (model_dir / TOKENS).read_text(encoding="utf-8").splitlines() if line.strip()}

    keyword_sets = {
        "upstream_test": (up / "test_wavs" / "keywords.txt").read_bytes().decode("utf-8"),
        "hr_wake": (model_dir / "keywords-wake.txt").read_bytes().decode("utf-8"),
        "hr_confirm": (model_dir / "keywords-confirm.txt").read_bytes().decode("utf-8"),
        "hr_hey": (model_dir / "keywords-hey-wake.txt").read_bytes().decode("utf-8"),
        "dense": dense_keywords_text(),
    }
    for name, text in keyword_sets.items():
        check_tokens(name, text, tokens)

    for sub, pattern in [("audio", "*.wav"), ("fbank", "*.f32le"), ("encoder", "*.f32le")]:
        (out / sub).mkdir(parents=True, exist_ok=True)
        for stale in (out / sub).glob(pattern):
            stale.unlink()

    wav_paths, audio_meta, wavs = {}, {}, {}
    for name in COPIED_WAVS:
        shutil.copyfile(up / "test_wavs" / name, out / "audio" / name)
        wav_paths[name] = (out / "audio" / name, f"kws/audio/{name}")
    for name in RIGHTKIT_WAVS:
        wav_paths[name] = (args.fixtures_dir / name, name)
    for name, (path, rel) in wav_paths.items():
        wavs[name] = load_wav(path)
        audio_meta[name] = {"path": rel, "samples": int(len(wavs[name])), "sha256": sha256_file(path)}

    cases = build_cases()
    used = set()
    with tempfile.TemporaryDirectory() as tmp:
        kw_files = {}
        for name, text in keyword_sets.items():
            kw_files[name] = Path(tmp) / f"{name}.txt"
            kw_files[name].write_bytes(text.encode("utf-8"))

        spotters = {}
        print(f"{'case':48} {'decodes':>7} {'hits':>4}  keywords")
        for case in cases:
            audio = case_audio(case, wavs)
            if case["feed_samples"] is None:
                case["feed_samples"] = int(len(audio))
            key = (case["keyword_set"], case["max_active_paths"], case["num_trailing_blanks"],
                   case["keywords_score"], case["keywords_threshold"])
            if key not in spotters:
                spotters[key] = make_spotter(sherpa_onnx, model_dir, kw_files[key[0]], *key[1:])
            case["decodes"], case["detections"] = run_case(spotters[key].keyword_spotter, case, audio)
            used.update(s for s in case["segments"] if isinstance(s, str))
            names = " ".join(d["keyword"] for d in case["detections"])
            print(f"{case['id']:48} {case['decodes']:7} {len(case['detections']):4}  {names[:120]}")

        fbank_low = make_spotter(sherpa_onnx, model_dir, kw_files["upstream_test"], 4, 1, 1.0, 0.25).keyword_spotter

    def write_fbank(key, frames):
        rel = f"fbank/{key.replace('.wav', '')}.f32le"
        (out / rel).write_bytes(frames.astype("<f4").tobytes())
        return {"audio": key, "frames": int(frames.shape[0]), "dim": 80, "file": rel, "sha256": sha256_file(out / rel)}

    max_diff = 0.0

    def checked_fbank(key, samples):
        nonlocal max_diff
        frames = sherpa_fbank(fbank_low, samples)
        ref = knf_fbank(samples)
        if ref.shape != frames.shape:
            sys.exit(f"fbank self-check: {key} sherpa {frames.shape} vs kaldi_native_fbank {ref.shape}")
        diff = float(np.max(np.abs(ref - frames)))
        max_diff = max(max_diff, diff)
        print(f"fbank {key}: {frames.shape[0]} frames, max |sherpa - kaldi_native_fbank| = {diff:.3e}")
        return frames

    fbank_entries = {}
    for name in FBANK_AUDIO:
        fbank_entries[name] = write_fbank(name, checked_fbank(name, wavs[name]))
        used.add(name)

    case_keys = {}
    for case in cases:
        key = audio_key(case)
        case_keys.setdefault(key, []).append(case)
    sizes = {key: len(case_audio(group[0], wavs)) for key, group in case_keys.items()}
    frame_bytes = {key: (0 if n < 280 else (n - 280) // 160 + 1) * 80 * 4 for key, n in sizes.items()}
    total = sum(e["frames"] * 80 * 4 for e in fbank_entries.values())
    total += sum(b for key, b in frame_bytes.items() if key not in fbank_entries)
    skip_long = total > FBANK_BUDGET_BYTES
    for key, group in case_keys.items():
        if skip_long and "long_38s.wav" in key.split("+"):
            for case in group:
                case["fbank"] = None
            continue
        if key not in fbank_entries:
            fbank_entries[key] = write_fbank(key, checked_fbank(key, case_audio(group[0], wavs)))
        for case in group:
            case["fbank"] = fbank_entries[key]["file"]
    fbank_bytes = sum(e["frames"] * 80 * 4 for e in fbank_entries.values())
    print(f"fbank: {len(fbank_entries)} files, {fbank_bytes} bytes" + (" (long_38s cases dropped)" if skip_long else ""))

    frames = sherpa_fbank(fbank_low, wavs[ENCODER_AUDIO])

    wheel_ort = wheel_onnxruntime_version(sherpa_onnx)
    enc, chunks, per_chunk, dim, py_ort = encoder_outputs(model_dir / ENCODER, frames, ENCODER_MAX_CHUNKS)
    enc_rel = f"encoder/{ENCODER_AUDIO[:-4]}.f32le"
    (out / enc_rel).write_bytes(enc.astype("<f4").tobytes())
    print(f"encoder {ENCODER_AUDIO}: {chunks} chunks x {per_chunk} x {dim}, python onnxruntime {py_ort}")

    reference = {
        "schema": SCHEMA,
        "reference": {
            "sherpa_onnx": f"{importlib.metadata.version('sherpa_onnx')} PyPI wheel (unpatched upstream)",
            "onnxruntime": wheel_ort,
            "kaldi_native_fbank": importlib.metadata.version("kaldi-native-fbank"),
            "platform": f"{platform.system().lower()}-{platform.machine()} GCC build",
            "generator": "tests/fixtures/kws/generate_reference.py",
        },
        "model": {
            "encoder": ENCODER, "encoder_sha256": sha256_file(model_dir / ENCODER),
            "decoder": DECODER, "decoder_sha256": sha256_file(model_dir / DECODER),
            "joiner": JOINER, "joiner_sha256": sha256_file(model_dir / JOINER),
            "tokens_sha256": sha256_file(model_dir / TOKENS),
        },
        "audio": {name: audio_meta[name] for name in wav_paths if name in used},
        "keyword_sets": keyword_sets,
        "fbank": list(fbank_entries.values()),
        "encoder": [{"audio": ENCODER_AUDIO, "chunks": chunks, "frames_per_chunk": per_chunk, "dim": dim,
                     "file": enc_rel, "onnxruntime": py_ort}],
        "cases": cases,
    }
    (out / "reference.json").write_text(render(reference) + "\n", encoding="utf-8")

    hits = sum(len(c["detections"]) for c in cases)
    print(f"{len(cases)} cases, {hits} detections; sherpa onnxruntime {wheel_ort}; "
          f"fbank max diff {max_diff:.3e}; wrote {out / 'reference.json'}")


if __name__ == "__main__":
    main()
