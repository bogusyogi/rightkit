import json,glob,collections,os,sys,re
sys.path.insert(0,os.path.dirname(__file__)); from merge import ALIAS,final,kind
S=os.path.dirname(__file__); EX=set(open(S+'/existing.txt').read().split())
NAMES={'postright':'PostRight','scraperight':'ScrapeRight','mailright':'MailRight','viewright':'ViewRight','genright':'GenRight','CutRight':'CutRight','pulse':'Pulse','heardright':'HeardRight','coderight':'CodeRight','legion':'Legion'}
COMMIT={r:os.popen(f'git -C /home/user/{r} rev-parse --short HEAD').read().strip() for r in NAMES}
ACTDESC={'NEW':'new RightKit package (or new piece of one)','EXTEND':'existing RightKit package gains this','ADOPT':'RightKit already ships it; delete app copy & use owner','SHELL':'thin app composition that stays in app','CONFIG':'app manifest/config that stays','DROP':'delete','VENDOR-OWN':'vendored 3rd-party: own/rewrite in RightKit','VENDOR-KEEP':'vendored 3rd-party: keep as plain dependency, delete vendored copy','NOTCODE':'docs/data, not code'}
def fix_action(t,a):
    # NEW vs EXTEND decided by whether final package exists in RightKit today
    if a in('NEW','EXTEND') and kind(t)!='other': return 'EXTEND' if t in EX else 'NEW'
    return a
allfiles={}
for r in NAMES:
    d=json.load(open(f'{S}/out/{r}-files.json'))
    for f in d['files']:
        f['origTarget']=f['target']; f['target']=final(f['target']); f['action']=fix_action(f['target'],f['action'])
    allfiles[r]=d['files']
def fmtn(n): return f'{n:,}'
os.makedirs(S+'/md',exist_ok=True)
# ---------- per repo
for r,files in allfiles.items():
    rows=collections.OrderedDict()
    for f in files:
        k=(f['target'],f['action'],f['capability'])
        x=rows.setdefault(k,{'files':0,'loc':0,'paths':[]}); x['files']+=1; x['loc']+=f['loc']; x['paths'].append(f['path'])
    tot=collections.Counter(); totf=collections.Counter()
    for f in files: tot[f['action']]+=f['loc']; totf[f['action']]+=1
    L=[f"# {NAMES[r]}: RightKit mapping\n",f"Repo `{r}` at `{COMMIT[r]}` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`{r}-rightkit-files.json`]({r}-rightkit-files.json).\n",
       "Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).\n",
       "## Summary\n","| Action | Meaning | Files | Lines |","|---|---|---:|---:|"]
    for a in ['NEW','EXTEND','ADOPT','VENDOR-OWN','VENDOR-KEEP','DROP','SHELL','CONFIG','NOTCODE']:
        if totf[a]: L.append(f"| {a} | {ACTDESC[a]} | {fmtn(totf[a])} | {fmtn(tot[a])} |")
    L.append(f"| **Total** | | **{fmtn(len(files))}** | **{fmtn(sum(tot.values()))}** |\n")
    moving=sum(tot[a] for a in ['NEW','EXTEND','ADOPT','VENDOR-OWN','VENDOR-KEEP','DROP'])
    L.append(f"Code leaving the app (NEW + EXTEND + ADOPT + vendor + DROP): **{fmtn(moving)} lines**. Staying as app shell/config: **{fmtn(tot['SHELL']+tot['CONFIG'])} lines** ({100*(tot['SHELL']+tot['CONFIG'])//max(1,sum(tot.values())-tot['NOTCODE'])}% of code).\n")
    L+=["## Mapping table\n","| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |","|---|---|---|---|---:|---:|---|"]
    order={'NEW':0,'EXTEND':1,'ADOPT':2,'VENDOR-OWN':3,'VENDOR-KEEP':4,'DROP':5,'SHELL':6,'CONFIG':7,'NOTCODE':8}
    for (t,a,c),x in sorted(rows.items(),key=lambda kv:(order[kv[0][1]],kv[0][0],-kv[1]['loc'])):
        dirs=sorted({os.path.dirname(p) or '.' for p in x['paths']})
        samp=', '.join(f'`{d}/`' for d in dirs[:3])+(f' +{len(dirs)-3} dirs' if len(dirs)>3 else '')
        L.append(f"| `{t}` | {kind(t)} | {a} | {c} | {x['files']} | {fmtn(x['loc'])} | {samp} |")
    sh=[f for f in files if f['action']=='SHELL']
    L+=["\n## What stays in the app (SHELL)\n","Entry points, command tables & app-specific wiring over RightKit packages. Largest files:\n","| File | Lines |","|---|---:|"]
    for f in sorted(sh,key=lambda f:-f['loc'])[:25]: L.append(f"| `{f['path']}` | {fmtn(f['loc'])} |")
    if len(sh)>25: L.append(f"| …{len(sh)-25} more (see JSON) | |")
    open(f"{S}/md/{r}-rightkit-mapping.md",'w').write('\n'.join(L)+'\n')
    json.dump({'repo':r,'commit':COMMIT[r],'generated':'2026-11-01','files':files},open(f"{S}/md/{r}-rightkit-files.json",'w'),indent=1)
# ---------- master
agg=collections.defaultdict(lambda:{'repos':collections.Counter(),'loc':0,'files':0,'actions':collections.Counter(),'caps':collections.OrderedDict(),'ext':collections.Counter(),'merged':set()})
for r,files in allfiles.items():
    for f in files:
        a=agg[f['target']]; a['repos'][r]+=f['loc']; a['loc']+=f['loc']; a['files']+=1; a['actions'][f['action']]+=f['loc']
        a['ext'][os.path.splitext(f['path'])[1]]+=f['loc']
        if f['origTarget']!=f['target']: a['merged'].add(f['origTarget'])
        if f['capability']!='tests': a['caps'].setdefault(f['capability'],set()).add(r)
json.dump({t:{'repos':dict(a['repos']),'loc':a['loc'],'files':a['files'],'actions':dict(a['actions']),'merged':sorted(a['merged']),'caps':{c:sorted(v) for c,v in a['caps'].items()},'ext':dict(a['ext'])} for t,a in agg.items()},open(S+'/md/master.json','w'),indent=1)
print('ok',len(agg))
