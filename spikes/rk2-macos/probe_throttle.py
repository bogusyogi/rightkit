#!/usr/bin/env python3
"""Page-only observation (no capture): does rAF stay at 60 Hz for 55 s in an off-screen window under each mitigation?"""
import os, sys, json
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'run.py')).read().split('if __name__')[0])
import tempfile
def trial(mods):
    tmp = tempfile.mkdtemp(dir=SCRATCH); suite = tmp + '/suite'
    env = dict(os.environ, RIGHTKIT_CONTROL_SERVICE="control-rk2", RIGHTKIT_SUITE_ROOT=suite)
    p = subprocess.Popen([FIX], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True); ALLPIDS.append(p.pid)
    sj = suite + "/runtime/control-rk2/service.json"
    while not os.path.exists(sj): time.sleep(.25)
    fx = Fix(suite); time.sleep(3)
    for m in mods: fx.cmd("vis_force", mode=m) if m in ("spi", "policy") else fx.cmd(m)
    fx.cmd("place", x=3600, y=300)
    time.sleep(56)
    rates = fx.ev("return window.__log.filter(l=>l.kind=='raf-rate').map(l=>Math.round(l.fps));")
    p.kill(); print(mods, rates, flush=True)
for mods in (["spi"], ["spi", "nap_off"], ["spi", "nap_off", "policy"]): trial(mods)
