#!/usr/bin/env python3
"""Compute the decision metrics for one evidence dir: analyze.py <evidence/label>"""
import json, os, re, subprocess, sys, statistics as st
ROOT = os.path.dirname(os.path.abspath(__file__)); HELPER = os.path.join(ROOT, "helper", "rk2-capture")
d = sys.argv[1]
run = json.load(open(f"{d}/run.json")); summ = json.load(open(f"{d}/summary.json")); fps = run["fps"]
frames = [json.loads(l) for l in open(f"{d}/frames.jsonl")]
scale = run["backing_scale"]; info = run["info_before_capture"]
top = info["frame"]["h"] - info["contentLayout"]["h"]
first_pts = summ.get("first_pts_host_s", 0)
# clock mapping: page epoch ms <-> uptime ns (two samples; drift = difference of offsets)
c0, c1 = run["clock0"], run["clock1"]
off0 = c0["epoch_ms"] * 1e6 - c0["uptime_ns"]; off1 = c1["epoch_ms"] * 1e6 - c1["uptime_ns"]
def epoch_to_up(ms): return ms * 1e6 - (off0 + (off1 - off0) / 2)
def up_to_enc(ns): return ns / 1e9 - first_pts
log = run["page_log"] or []
cyc0 = next((l["t0Epoch"] for l in log if l["kind"] == "cycle0"), None)
clicks = [(l["epochMs"], l["step"]) for l in log if l["kind"] == "click:next"]
posts = [(l["epochMs"], l["step"]) for l in log if l["kind"] == "postcondition:step"]
# OCR windows: every frame from -0.2 to +2.5 s around each postcondition
wins = ",".join(f"{up_to_enc(epoch_to_up(t))-0.2:.3f}:{up_to_enc(epoch_to_up(t))+2.5:.3f}" for t, _ in posts)
vid = f"{d}/capture.mp4"; an = f"{d}/analysis.jsonl"
if os.path.exists(vid) and os.path.getsize(vid) > 0:
    r = subprocess.run([HELPER, "analyze", "--video", vid, "--out", an, "--scale", str(scale), "--content-top", str(top), "--ocr-ms", "500", "--ocr-windows", wins], capture_output=True, text=True)
    run_an = r.stdout.strip()
else: run_an = "no video"
fr = [json.loads(l) for l in open(an)] if os.path.exists(an) else []
M = {"label": run["label"], "frames_callbacks": summ.get("callbacks"), "frames_complete": summ.get("complete"), "frames_idle": summ.get("idle"), "frames_blank_status": summ.get("blank"),
     "decoded_frames": len(fr), "file_bytes": summ.get("file_bytes"), "analyzer": run_an}
if fr:
    luma_black = [f for f in fr if f["mean_luma"] < 15]
    M["black_frames_luma_lt_15"] = len(luma_black)
    M["strip_valid_frames"] = sum(f["strip_valid"] for f in fr)
    M["max_foreign_magenta_samples"] = max(f["magenta"] for f in fr)
    M["frames_with_magenta_gt_20"] = sum(f["magenta"] > 20 for f in fr)
    # animated-truth per frame
    def animating(f):
        if cyc0 is None: return None
        ep = (f["pts"] + first_pts) * 1e9; ms = None
        # frame pts -> uptime ns -> page epoch ms
        up_ns = ep; ms = (up_ns + (off0 + (off1 - off0) / 2)) / 1e6
        c = ((ms - cyc0) / 1000.0) % 10
        return 0.4 < c < 6.6
    anim = [f for f in fr if animating(f)]
    pairs = [(a, b) for a, b in zip(fr, fr[1:]) if animating(a) and animating(b) and a["strip_valid"] and b["strip_valid"]]
    adv = sum(1 for a, b in pairs if b["counter"] != a["counter"])
    M["animated_frame_intervals"] = len(pairs); M["animated_intervals_counter_advanced"] = adv
    M["pct_intervals_advanced"] = round(100 * adv / len(pairs), 1) if pairs else None
    # fixed-rate resample ticks
    ticks = []; j = 0; prev = None; adv2 = tot2 = 0
    if fr:
        t = fr[0]["pts"]
        while t < fr[-1]["pts"]:
            while j + 1 < len(fr) and fr[j + 1]["pts"] <= t: j += 1
            f = fr[j]
            if animating(f) and f["strip_valid"]:
                if prev is not None:
                    tot2 += 1; adv2 += f["counter"] != prev
                prev = f["counter"]
            else: prev = None
            t += 1.0 / fps
    M["resampled_ticks"] = tot2; M["pct_ticks_advanced"] = round(100 * adv2 / tot2, 1) if tot2 else None
    # split by the page's own rAF rate (2 s samples): 'page60' = page painting >=45 Hz, 'throttled' = below
    rr = sorted((l["epochMs"], l["fps"]) for l in log if l["kind"] == "raf-rate")
    def page_rate(f):
        ms = ((f["pts"] + first_pts) * 1e9 + (off0 + (off1 - off0) / 2)) / 1e6
        for e, v in rr:
            if e >= ms: return v
        return rr[-1][1] if rr else None
    seg = {"page60": [0, 0], "throttled": [0, 0]}
    prevc = None; prevk = None
    for a, b in zip(fr, fr[1:]):
        if animating(a) and animating(b) and a["strip_valid"] and b["strip_valid"]:
            pr = page_rate(b); k = "page60" if (pr or 0) >= 45 else "throttled"
            seg[k][0] += 1; seg[k][1] += b["counter"] != a["counter"]
    M["intervals_by_page_rate"] = {k: {"intervals": v[0], "advanced": v[1], "pct": round(100 * v[1] / v[0], 1) if v[0] else None} for k, v in seg.items()}
    thr = [round(f["pts"], 1) for f in fr if (page_rate(f) or 99) < 30]
    M["first_throttled_pts_s"] = thr[0] if thr else None
    M["page_raf_rate_samples"] = [round(v) for _, v in rr]
    cs = [f["counter"] for f in fr if f["strip_valid"]]
    M["counter_first_last"] = [cs[0], cs[-1]] if cs else None
    M["distinct_counter_values"] = len(set(cs))
    # delivered frame rate during animation
    if anim: 
        span = anim[-1]["pts"] - anim[0]["pts"]
    # OCR
    ocr = [(f["pts"], " ".join(f.get("ocr", []))) for f in fr if "ocr" in f]
    def expected(pts):
        ms = (pts + first_pts) * 1e9; ms = (ms + (off0 + (off1 - off0) / 2)) / 1e6
        s = 1
        for t, stp in clicks:
            if t <= ms - 400: s = stp
        return s
    ok = tot = 0; stable = []
    for pts, txt in ocr:
        m = re.search(r"STEP[-–—_ ]?(\d{3})", txt.replace("O", "0"))
        got = int(m.group(1)) if m else None
        # skip samples within 700 ms after a change (transition)
        ms = ((pts + first_pts) * 1e9 + (off0 + (off1 - off0) / 2)) / 1e6
        if any(0 <= ms - t <= 700 for t, _ in clicks): continue
        tot += 1; ok += got == expected(pts)
    M["ocr_samples_stable"] = tot; M["ocr_step_correct"] = ok
    # per-step recovery latency: postcondition (page, painted) -> first OCR showing new step
    lat = []
    for t, stp in posts:
        pe = up_to_enc(epoch_to_up(t))
        hit = None
        for pts, txt in ocr:
            m = re.search(r"STEP[-–—_ ]?(\d{3})", txt.replace("O", "0"))
            if pts >= pe - 0.2 and m and int(m.group(1)) == stp: hit = pts; break
        lat.append({"step": stp, "post_enc_s": round(pe, 3), "ocr_first_seen_enc_s": hit, "latency_ms": None if hit is None else round((hit - pe) * 1000)})
    M["step_recovery"] = lat
# inputs: admission/dispatch/completion
gj = [json.loads(l) for l in open(f"{d}/gate-journal.jsonl")] if os.path.exists(f"{d}/gate-journal.jsonl") else []
rows = []
for inp in run["inputs"]:
    ev = [e for e in gj if e["method"] == inp["method"] and e["uptime_ns"] >= inp["caller_send_ns"] - 1e6 and e["uptime_ns"] <= inp["caller_recv_ns"] + 1e6]
    g = {e["ev"]: e["uptime_ns"] for e in ev}
    t0 = inp["caller_send_ns"]
    rows.append({"input": inp["name"], "method": inp["method"], "ok": isinstance(inp["result"], dict) and inp["result"].get("ok", False),
                 "caller_send_ms": 0, "admit_ms": (g["admit"] - t0) / 1e6 if "admit" in g else None,
                 "dispatch_ms": (g["dispatch"] - t0) / 1e6 if "dispatch" in g else None,
                 "complete_ms": (g["complete"] - t0) / 1e6 if "complete" in g else None,
                 "caller_recv_ms": (inp["caller_recv_ns"] - t0) / 1e6,
                 "send_enc_pts_s": round(up_to_enc(t0), 3), "admit_enc_pts_s": round(up_to_enc(g["admit"]), 3) if "admit" in g else None,
                 "complete_enc_pts_s": round(up_to_enc(g["complete"]), 3) if "complete" in g else None})
M["inputs"] = rows
M["foreground_changes"] = [e for e in summ.get("foreground_events", []) if e["kind"] == "change"]
M["frontmost_before"] = run["frontmost_before"]["info"][:90]; M["frontmost_after"] = run["frontmost_after"]["info"][:90]
M["clock_anchor_offset_host_vs_uptime_ms"] = [round((a["host_clock_s"] * 1e9 - a["uptime_ns"]) / 1e6, 3) for a in summ["clock_anchors"][:1] + summ["clock_anchors"][-1:]]
M["epoch_uptime_drift_ms"] = round((off1 - off0) / 1e6, 3)
json.dump(M, open(f"{d}/metrics.json", "w"), indent=1)
print(json.dumps({k: v for k, v in M.items() if k not in ("inputs", "step_recovery")}, indent=1))
