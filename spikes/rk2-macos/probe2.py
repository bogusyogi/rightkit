#!/usr/bin/env python3
"""rAF investigation harness. usage: probe2.py SECONDS POLL_S mod [mod...]   (POLL_S=0: no polling, one read at end)
mods: spi policy nap_off  (existing) plus any fixture command name given as cmd:NAME; env vars passed through."""
import os, sys, json
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'run.py')).read().split('if __name__')[0])
import tempfile
secs = float(sys.argv[1]); poll = float(sys.argv[2]); mods = sys.argv[3:]
tmp = tempfile.mkdtemp(dir=SCRATCH); suite = tmp + '/suite'
env = dict(os.environ, RIGHTKIT_CONTROL_SERVICE="control-rk2", RIGHTKIT_SUITE_ROOT=suite)
p = subprocess.Popen([FIX], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True); ALLPIDS.append(p.pid)
try:
    sj = suite + "/runtime/control-rk2/service.json"
    while not os.path.exists(sj): time.sleep(.25)
    fx = Fix(suite); time.sleep(3)
    def parse(m):
        at = None
        if "@" in m: a_, m = m.split("@", 1); at = float(a_)
        parts = m.split(";"); kw = dict(p.split("=", 1) for p in parts[1:]); return at, parts[0], kw
    timed = []
    for m in mods:
        at, name, kw = parse(m)
        if at is not None: timed.append((at, name, kw)); continue
        if name in ("spi", "policy"): print(m, fx.cmd("vis_force", mode=name))
        else: print(m, fx.cmd(name, **kw))
    timed.sort(key=lambda x: x[0])
    time.sleep(float(os.environ.get("PREPLACE", "0")))
    info = fx.cmd("place", x=3600, y=300); print("occ", info["occlusionState"], "onAnyScreen", info["onAnyScreen"], flush=True)
    MON = """if(!window.__m){window.__m={t:[],r:[],mc:[],last:performance.now()};let a=0,b=0,c=0;
    setInterval(()=>{a++},16);
    const ch=new MessageChannel();ch.port1.onmessage=()=>{b++;setTimeout(()=>ch.port2.postMessage(0),0)};ch.port2.postMessage(0);
    (function f(){c++;requestAnimationFrame(f)})();
    setInterval(()=>{const n=performance.now(),d=(n-window.__m.last)/1000;window.__m.last=n;window.__m.t.push(Math.round(a/d));window.__m.mc.push(Math.round(b/d));window.__m.r.push(Math.round(c/d));a=b=c=0},1000);}
    return 1;"""
    fx.ev(MON)
    t0 = time.time(); occ = []
    while time.time() - t0 < secs:
        while timed and time.time() - t0 >= timed[0][0]:
            at, name, kw = timed.pop(0); print(round(time.time()-t0), "ACTION", name, kw, fx.cmd(name, **kw), flush=True)
        if poll:
            time.sleep(poll); i = fx.cmd("win_info"); occ.append((round(time.time()-t0), i["occlusionState"]))
        else: time.sleep(1)
    rates = m_r = None
    rates = fx.ev("return window.__log.filter(l=>l.kind=='raf-rate').map(l=>Math.round(l.fps));")
    m = fx.ev("return window.__m;")
    print("rAF/s", m["r"]); print("timer16/s (first10)", m["t"][:10])
    print("poll", poll, "mods", mods); print("rAF/2s", rates)
    print("occ changes", sorted(set(o[1] for o in occ)) if occ else "n/a")
    low = [i for i, r in enumerate(rates) if r < 50 and i > 2]
    print("first throttled (s):", (low[0]*2 if low else None), " frac>=50Hz after warmup:", round(sum(r>=50 for r in rates[3:])/max(1,len(rates[3:])),2))
finally:
    for q in ALLPIDS:
        try: os.kill(q, 9)
        except Exception: pass
    shutil.rmtree(tmp, ignore_errors=True)
