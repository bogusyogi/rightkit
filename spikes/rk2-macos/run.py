#!/usr/bin/env python3
"""RK2 Spike A driver (spike harness, not production). One run = one visibility state at one fps.
Usage: run.py <state> <fps> <seconds> [mode]    state: offscreen|occluded|apphidden|minimized"""
import json, os, re, shutil, subprocess, sys, tempfile, threading, time

ROOT = os.path.dirname(os.path.abspath(__file__))
B = "/Volumes/D/.rightkit-managed/rightkit-build-control/ws/"
FIX = os.environ.get("RK2_FIXTURE_BIN", B + "b9492cf939fc/g/001/target/aarch64-apple-darwin/debug/rk2-fixture")
SVC = os.environ.get("RK2_SVC_CALL", B + "54747347a461/g/001/target/aarch64-apple-darwin/debug/svc_call")
HELPER = os.path.join(ROOT, "helper", "rk2-capture")
EVID = os.path.join(ROOT, "evidence")
SCRATCH = os.environ.get("RK2_SCRATCH", "/private/tmp/claude-501/-Volumes-D-claude/c46e6e5c-718c-4d68-aeb6-419633959f27/scratchpad")

ALLPIDS = []
def up(): return time.clock_gettime_ns(time.CLOCK_UPTIME_RAW)

def sh(*a, **k): return subprocess.run(a, capture_output=True, text=True, **k)

def frontmost():
    asn = sh("lsappinfo", "front").stdout.strip()
    info = sh("lsappinfo", "info", "-only", "bundleid", "-only", "pid", asn).stdout.strip().replace("\n", " ")
    return {"asn": asn, "info": info}

def make_bundle(d, name, bid):
    app = os.path.join(d, name + ".app")
    os.makedirs(app + "/Contents/MacOS", exist_ok=True)
    shutil.copy(FIX, app + "/Contents/MacOS/rk2-fixture")
    open(app + "/Contents/Info.plist", "w").write(
        f'<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>rk2-fixture</string>'
        f'<key>CFBundleIdentifier</key><string>{bid}</string><key>CFBundleName</key><string>{name}</string>'
        f'<key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>1</string></dict></plist>')
    return app

class Fix:
    def __init__(self, suite): self.suite = suite
    def call(self, method, params=None):
        t0 = up()
        r = subprocess.run([SVC, self.suite, "control-rk2", "right-qa", method, json.dumps(params or {})], capture_output=True, text=True)
        t1 = up()
        try: v = json.loads(r.stdout)
        except Exception: v = {"raw": r.stdout, "err": r.stderr}
        return v, t0, t1
    def cmd(self, name, **kw):
        v, _, _ = self.call("command", dict(name=name, **kw))
        r = v.get("result", v) if isinstance(v, dict) else v
        if isinstance(r, str):
            try: r = json.loads(r)
            except Exception: pass
        return r
    def ev(self, js):
        v, _, _ = self.call("eval", {"js": js})
        return v.get("value") if isinstance(v, dict) else None

def excl_all(): return ",".join(str(p) for p in ALLPIDS)

def run(state, fps, seconds, mode="stream"):
    PRE = os.environ.get("RK2_PRE", "spi")
    label = f"{state}-{fps}fps-{mode}" + ("" if PRE == "spi" else "-nospi") + ("-noinput" if os.environ.get("RK2_NOINPUT") else "")
    out = os.path.join(EVID, label); shutil.rmtree(out, ignore_errors=True); os.makedirs(out)
    tmp = tempfile.mkdtemp(prefix="rk2run.", dir=SCRATCH)
    suite = os.path.join(tmp, "suite")
    rep = {"label": label, "state": state, "fps": fps, "seconds": seconds, "mode": mode}
    front0 = frontmost(); rep["frontmost_before"] = front0
    logf = os.path.join(out, "gate-journal.jsonl")
    # Direct exec of the fixture binary (not `open`): LaunchServices-launched .app copies of this fixture stopped creating
    # their configured window mid-session (window_not_found for >12 s), the bare binary always did. See results note.
    env = dict(os.environ, RIGHTKIT_CONTROL_SERVICE="control-rk2", RIGHTKIT_SUITE_ROOT=suite, RK2_LOG=logf)
    proc = subprocess.Popen([FIX], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    ALLPIDS.append(proc.pid)
    sj = os.path.join(suite, "runtime", "control-rk2", "service.json")
    for _ in range(80):
        if os.path.exists(sj): break
        time.sleep(0.25)
    else: rep["error"] = "fixture service never appeared"; return rep, tmp, []
    pid = json.load(open(sj))["pid"]; pids = [pid]; ALLPIDS.append(pid)
    fx = Fix(suite)
    for _ in range(60):
        if fx.ev("return document.readyState;") == "complete": break
        time.sleep(0.25)
    info = fx.cmd("win_info"); rep["info_initial"] = info
    scr = info["screens"]; maxx = max(s["x"] + s["w"] for s in scr); union = [min(s["x"] for s in scr), min(s["y"] for s in scr), maxx, max(s["y"] + s["h"] for s in scr)]
    px, py = maxx + 2000, 300
    rep["display_union_appkit"] = union; rep["placement_target"] = [px, py]
    rep["pre_visibility_force"] = PRE
    if PRE == "spi": rep["vis_force_result"] = fx.cmd("vis_force", mode="spi")
    info = fx.cmd("place", x=px, y=py); rep["info_placed"] = info
    f = info["frame"]; disjoint = all(f["x"] >= s["x"] + s["w"] or f["x"] + f["w"] <= s["x"] or f["y"] >= s["y"] + s["h"] or f["y"] + f["h"] <= s["y"] for s in scr)
    rep["placement_outside_all_displays"] = disjoint; rep["backing_scale"] = info["backingScale"]
    time.sleep(1.0)
    occ_app = None
    if state == "occluded":
        oenv = dict(os.environ, RK2_OCCLUDER=f"{px-60},{py-60},{f['w']+120},{f['h']+120}")
        oenv.pop("RIGHTKIT_CONTROL_SERVICE", None)
        occ_app = subprocess.Popen([FIX], env=oenv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        ALLPIDS.append(occ_app.pid)
        time.sleep(4.0)
    elif state == "apphidden": rep["info_state"] = fx.cmd("app_hide")
    elif state == "minimized": rep["info_state"] = fx.cmd("miniaturize")
    time.sleep(1.5)
    info = fx.cmd("win_info"); rep["info_before_capture"] = info; print("INFO", str(info)[:300], flush=True)
    rep["page_before_capture"] = fx.ev("return {vis:document.visibilityState,focus:document.hasFocus(),counter:document.getElementById('frame').textContent};")
    time.sleep(0.6)
    rep["page_before_capture_2"] = fx.ev("return {vis:document.visibilityState,counter:document.getElementById('frame').textContent};")
    rep["sck_list"] = json.loads(sh(HELPER, "list", "--pid", str(pid)).stdout or "{}")
    # element geometry in viewport CSS px
    g = fx.ev("const r=id=>{const b=document.getElementById(id).getBoundingClientRect();return [b.x+b.width/2,b.y+b.height/2,b.x,b.width]};return {next:r('next'),inp:r('inp'),scrub:r('scrub'),sc:r('sc')};")
    occ_pids = []
    if occ_app: occ_pids = [occ_app.pid]
    if occ_app:
        z = json.loads(sh(HELPER, "zorder", "--pids", f"{pid},{occ_app.pid}").stdout or "{}")
        z["windows"] = [w for w in z.get("windows", []) if w["bounds"]["w"] >= 700]; rep["zorder_target_and_occluder"] = z
        ow = next((w for w in z["windows"] if w["pid"] == occ_app.pid), None)
        if ow:  # sanity: the occluder really renders magenta (recorded on its own, 4 s)
            sd = os.path.join(out, "occluder-sanity"); os.makedirs(sd, exist_ok=True)
            subprocess.run([HELPER, "record", "--window-id", str(ow["windowNumber"]), "--fps", "30", "--seconds", "4", "--scale", "1", "--out", sd, "--exclude-pids", excl_all()], capture_output=True)
            a = subprocess.run([HELPER, "analyze", "--video", sd + "/capture.mp4", "--out", sd + "/analysis.jsonl", "--scale", "1", "--content-top", "28", "--ocr-ms", "100000"], capture_output=True, text=True)
            mg = [json.loads(l)["magenta"] for l in open(sd + "/analysis.jsonl")]
            rep["occluder_sanity"] = {"frames": len(mg), "max_magenta_samples": max(mg) if mg else None}
    excl = ",".join(str(p) for p in pids + occ_pids)
    clk0 = fx.cmd("clock")
    cmd = [HELPER, "record", "--window-id", str(info["windowNumber"]), "--fps", str(fps), "--seconds", str(seconds), "--scale", str(info["backingScale"]),
           "--out", out, "--mode", mode, "--exclude-pids", excl]
    helper = subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True)
    lines = []
    started = threading.Event(); first = threading.Event()
    def rd():
        for ln in helper.stdout:
            lines.append((up(), ln.strip()))
            if '"capture_started"' in ln: started.set()
            if '"first_accepted_frame"' in ln: first.set()
            if '"ok":false' in ln.replace(" ", ""): started.set(); first.set()
    threading.Thread(target=rd, daemon=True).start()
    started.wait(25); t_start = up()
    rep["capture_started_seen"] = started.is_set()
    first.wait(8 if mode == "stream" else 0.1)
    rep["first_frame_seen"] = first.is_set()
    inputs = []
    if started.is_set() and not any('"ok":false' in l.replace(" ", "") for _, l in lines):
        N, I, S, C = g["next"], g["inp"], g["scrub"], g["sc"]
        sched = [
            (6, "click Next", "click", {"x": N[0], "y": N[1]}),
            (14, "click input", "click", {"x": I[0], "y": I[1]}),
            (15.5, "type", "type", {"text": "hello rk2"}),
            (22, "click Next", "click", {"x": N[0], "y": N[1]}),
            (30, "drag scrub", "drag", {"x1": S[2] + 20, "y1": S[1], "x2": S[2] + S[3] - 20, "y2": S[1], "steps": 14}),
            (38, "wheel scroll", "wheel", {"x": C[0], "y": C[1], "dx": 0, "dy": -120}),
            (44, "key Right", "key", {"key": "right"}),
            (50, "click Next", "click", {"x": N[0], "y": N[1]}),
        ]
        for off, name, method, params in sched:
            if off + 2 > seconds or os.environ.get("RK2_NOINPUT"): continue
            dl = t_start + off * 1e9
            while up() < dl: time.sleep(0.01)
            v, t0, t1 = fx.call(method, params)
            inputs.append({"name": name, "method": method, "offset_s": off, "caller_send_ns": t0, "caller_recv_ns": t1, "result": v})
    helper.wait(timeout=seconds + 60)
    rep["helper_exit"] = helper.returncode
    rep["helper_lines"] = [l for _, l in lines][:6]
    rep["inputs"] = inputs
    clk1 = fx.cmd("clock"); rep["clock0"] = clk0; rep["clock1"] = clk1
    rep["page_log"] = fx.ev("return window.__log;")
    rep["info_after"] = fx.cmd("win_info")
    rep["frontmost_after"] = frontmost()
    json.dump(rep, open(os.path.join(out, "run.json"), "w"), indent=1)
    return rep, tmp, pids + occ_pids

if __name__ == "__main__":
    state, fps, secs = sys.argv[1], int(sys.argv[2]), float(sys.argv[3])
    mode = sys.argv[4] if len(sys.argv) > 4 else "stream"
    os.makedirs(EVID, exist_ok=True)
    rep, tmp, pids = {}, None, []
    try:
        rep, tmp, pids = run(state, fps, secs, mode)
    finally:
        pids = list(set(pids + ALLPIDS))
        for p in pids:
            try: os.kill(p, 15)
            except Exception: pass
        time.sleep(1)
        for p in pids:
            try: os.kill(p, 9)
            except Exception: pass
        if tmp: shutil.rmtree(tmp, ignore_errors=True)
    print(json.dumps({k: rep.get(k) for k in ["label", "error", "placement_outside_all_displays", "backing_scale", "capture_started_seen", "first_frame_seen", "helper_exit", "helper_lines", "frontmost_before", "frontmost_after"]}, indent=1))
