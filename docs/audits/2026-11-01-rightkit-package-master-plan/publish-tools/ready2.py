import re,sys,subprocess,json,urllib.request,tomllib,os
cache={}
def idx(n):
    if n in cache: return cache[n]
    p = n if len(n)<=2 else ("3/"+n[0]+"/"+n if len(n)==3 else n[:2]+"/"+n[2:4]+"/"+n)
    try:
        r=urllib.request.Request("https://index.crates.io/"+p,headers={"User-Agent":"x"})
        t=urllib.request.urlopen(r).read().decode()
        v=[json.loads(l)["vers"] for l in t.splitlines() if l.strip()]
    except Exception: v=[]
    cache[n]=v; return v
def ver(c):
    return tomllib.load(open(f"crates/{c}/Cargo.toml","rb"))["package"]["version"]
pend=[l.split()[0] for l in open(sys.argv[1])]
for c in pend:
    try: t=tomllib.load(open(f"crates/{c}/Cargo.toml","rb"))
    except Exception as e: print(c,"??"); continue
    v=t["package"]["version"]
    if v in idx(c): continue
    miss=[]
    for sec in ("dependencies","build-dependencies"):
        for k,d in t.get(sec,{}).items():
            if isinstance(d,dict) and "path" in d and not d.get("optional"):
                dv=ver(os.path.basename(d["path"]))
                if dv not in idx(k): miss.append(f"{k}@{dv}")
    print(c,v,"READY" if not miss else "needs "+",".join(miss))
