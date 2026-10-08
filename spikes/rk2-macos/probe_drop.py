#!/usr/bin/env python3
import os, sys, json
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'run.py')).read().split('if __name__')[0])
import tempfile
with_cap = sys.argv[1] == "cap"
tmp = tempfile.mkdtemp(dir=SCRATCH); suite = tmp + '/suite'
env = dict(os.environ, RIGHTKIT_CONTROL_SERVICE="control-rk2", RIGHTKIT_SUITE_ROOT=suite)
p = subprocess.Popen([FIX], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True); ALLPIDS.append(p.pid)
sj = suite + "/runtime/control-rk2/service.json"
while not os.path.exists(sj): time.sleep(.25)
fx = Fix(suite); time.sleep(3)
mods = sys.argv[2:]
fx.cmd("vis_force", mode="spi")
for m in mods:
    if m in ("nap_off",): fx.cmd(m)
    if m == "policy": fx.cmd("vis_force", mode="policy")
info = fx.cmd("place", x=3600, y=300); time.sleep(2)
helper = None
if with_cap:
    info = fx.cmd("win_info")
    helper = subprocess.Popen([HELPER, "record", "--window-id", str(info["windowNumber"]), "--fps", "30", "--seconds", "50", "--scale", "1", "--out", tmp + "/cap", "--exclude-pids", str(p.pid)], stdout=subprocess.DEVNULL)
t0 = time.time()
if 'late' in mods: time.sleep(20)
if 'late10' in mods: time.sleep(10)
STEP = 1 if 'fast' in mods else 3
while time.time() - t0 < 60:
    time.sleep(STEP)
    i = fx.cmd("win_info") if "evalonly" not in mods else {"occlusionState": 0, "isVisible": 0}
    pg = fx.ev("const l=window.__log.filter(l=>l.kind=='raf-rate');return [document.visibilityState,Math.round(l[l.length-1].fps)];")
    top = []
    print(round(time.time() - t0), pg, "occl", i["occlusionState"], "vis", i["isVisible"], "top", [(a[1], a[2][-30:]) for a in top], flush=True)
if helper: helper.wait()
p.kill()
