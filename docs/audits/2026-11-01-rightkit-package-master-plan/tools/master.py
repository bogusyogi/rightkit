import json,os,collections,sys
S=os.path.dirname(__file__); sys.path.insert(0,S); from merge import ALIAS,kind
m=json.load(open(S+'/md/master.json')); EX=set(open(S+'/existing.txt').read().split())
N={'postright':'PostRight','scraperight':'ScrapeRight','mailright':'MailRight','viewright':'ViewRight','genright':'GenRight','CutRight':'CutRight','pulse':'Pulse','heardright':'HeardRight','coderight':'CodeRight','legion':'Legion'}
FAM=[('Agent platform',['rightkit-agent-engine','rightkit-agent-server','rightkit-agent-orchestration','rightkit-agent-policy','rightkit-agent-gateway','rightkit-approvals','rightkit-skills','@rightkit/skills','rightkit-evidence','rightkit-scheduler','rightkit-bot-messaging','rightkit-github','rightkit-chat-bridge','rightkit-ai-accounts','rightkit-agent-demo','RightKitAgent','@rightkit/agent-ui','rightkit-terminal-ui','@rightkit/terminal-ui']),
 ('Context, memory & code intelligence',['rightkit-context','rightkit-memory','rightkit-adapt','rightkit-repo-graph','rightkit-code-audit','rightkit-research']),
 ('Dictation & voice',['rightkit-dictation','rightkit-text-polish','rightkit-voice-commands','RightKitDictation','@rightkit/dictation-ui']),
 ('Video editing',['rightkit-timeline','rightkit-video-project','rightkit-compositor','rightkit-autoedit','rightkit-shorts','rightkit-reframe','rightkit-captions','rightkit-color','RightKitMedia','@rightkit/video-ui']),
 ('Generative media',['rightkit-media-gen','rightkit-local-gen','rightkit-prompt-recipes','rightkit-media-pipeline','rightkit-remote-compute','rightkit-blender-worker','@rightkit/media-gen-ui']),
 ('Content pipeline & social',['rightkit-pipeline','rightkit-content-plan','rightkit-content-proof','rightkit-social-publish','rightkit-social-analytics','rightkit-social-inbox','rightkit-social-mock','@rightkit/social-ui']),
 ('Ingest & library',['rightkit-media-fetch','rightkit-web-extract','rightkit-web-session','rightkit-library','RightKitLibrary','@rightkit/library-ui']),
 ('Documents',['@rightkit/document-workspace','rightkit-document-workspace','@rightkit/markdown-editor','@rightkit/html-editor','@rightkit/docx-editor']),
 ('Mail & workspace',['rightkit-mail','rightkit-google-apis','@rightkit/mail-ui','@rightkit/workspace-ui']),
 ('System utilities',['rightkit-mesh','rightkit-system-monitor','rightkit-disk-cleanup','rightkit-app-manager','rightkit-launcher','rightkit-clipboard-history','rightkit-desktop-tweaks','rightkit-privileged-helper','rightkit-app-lock','RightKitNotch','RightKitSystemUI','@rightkit/system-ui']),
 ('Foundation & tooling',['rightkit-doctor','rightkit-bench','rightkit-ml-lab','@rightkit/config','@rightkit/waitlist-worker']),
]
fn=lambda n:f'{n:,}'
def caps(a,k=6):
    c=[x for x in a['caps'] if x not in('tests','e2e tests')]
    return '; '.join(c[:k])+(f' (+{len(c)-k} more)' if len(c)>k else '')
def langs(a):
    e=a['ext']; s=[]
    if any(e.get(x) for x in('.swift','.m','.mm','.h')): s.append('Swift')
    if any(e.get(x) for x in('.kt','.kts','.gradle')): s.append('Gradle')
    if e.get('.py'): s.append('Python')
    return ', '.join(s)
new=[t for t,a in m.items() if a['actions'].get('NEW') and kind(t)!='other']
L=["# RightKit package master plan","","Date: 2026-11-01. Built by going through all 10 app repositories file by file (every tracked code file on `main`), mapping each to the RightKit package it should become or use, then merging overlaps across apps. Per-app tables: `docs/rightkit-inventory/<app>-rightkit-mapping.md` in each app repo (plus a JSON with every file).","","Goal: no app code outside RightKit except a thin app shell (entry points, command tables, wiring). Package names are product-neutral; a package used by only one app today is still a RightKit package.",""]
# totals per app
tot={}
for r in N:
    d=json.load(open(f'{S}/md/{r}-rightkit-files.json'))['files']; c=collections.Counter()
    for f in d: c[f['action']]+=f['loc']
    tot[r]=c
L+=["## Coverage by app","","| App | Code lines | NEW | EXTEND | ADOPT | Vendor | DROP | Stays (shell+config) | Stays % |","|---|---:|---:|---:|---:|---:|---:|---:|---:|"]
G=collections.Counter()
for r,c in tot.items():
    code=sum(c.values())-c['NOTCODE']; st=c['SHELL']+c['CONFIG']; G.update(c)
    L.append(f"| {N[r]} | {fn(code)} | {fn(c['NEW'])} | {fn(c['EXTEND'])} | {fn(c['ADOPT'])} | {fn(c['VENDOR-OWN']+c['VENDOR-KEEP'])} | {fn(c['DROP'])} | {fn(st)} | {100*st//code}% |")
code=sum(G.values())-G['NOTCODE']; st=G['SHELL']+G['CONFIG']
L.append(f"| **All** | **{fn(code)}** | **{fn(G['NEW'])}** | **{fn(G['EXTEND'])}** | **{fn(G['ADOPT'])}** | **{fn(G['VENDOR-OWN']+G['VENDOR-KEEP'])}** | **{fn(G['DROP'])}** | **{fn(st)}** | **{100*st//code}%** |")
L+=["","Lines include tests, which move with the code they test.",""]
# final new list
crates=[t for t in new if kind(t)=='crate']; npm=[t for t in new if kind(t)=='npm']; sw=[t for t in new if kind(t)=='Swift']
L+=["## Final list: new RightKit packages to create & publish","",f"**{len(new)} new packages**: {len(crates)} crates, {len(npm)} npm packages, {len(sw)} Swift packages. Grouped by family; within a family, ordered by size.",""]
seen=set()
for fam,members in FAM:
    mem=[t for t in members if t in new]; seen.update(mem)
    if not mem: continue
    L+=[f"### {fam}","","| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |","|---|---|---|---:|---|---|---|"]
    for t in sorted(mem,key=lambda t:-m[t]['loc']):
        a=m[t]; L.append(f"| `{t}` | {kind(t)} | {', '.join(N[r] for r in a['repos'])} | {fn(a['loc'])} | {langs(a)} | {', '.join('`'+x+'`' for x in a['merged'])} | {caps(a)} |")
    L.append("")
miss=[t for t in new if t not in seen]
if miss: L+=["### Other",""]+[f"- `{t}` ({', '.join(N[r] for r in m[t]['repos'])}, {fn(m[t]['loc'])} lines): {caps(m[t])}" for t in miss]+[""]
# extend
ext=[t for t,a in m.items() if a['actions'].get('EXTEND') and kind(t)!='other']
L+=["## Existing RightKit packages to extend","",f"**{len(ext)} existing packages** gain capability now living in apps.","","| Package | Used by | Lines moving in | What it gains |","|---|---|---:|---|"]
for t in sorted(ext,key=lambda t:-m[t]['actions']['EXTEND']):
    a=m[t]; L.append(f"| `{t}` | {', '.join(N[r] for r in a['repos'])} | {fn(a['actions']['EXTEND'])} | {caps(a,5)} |")
ad=[t for t,a in m.items() if a['actions'].get('ADOPT') and kind(t)!='other']
L+=["","## Already in RightKit: delete app copies & adopt","","| Package | Apps still carrying own copy | Lines to delete |","|---|---|---:|"]
for t in sorted(ad,key=lambda t:-m[t]['actions']['ADOPT']):
    a=m[t]; L.append(f"| `{t}` | {', '.join(N[r] for r in a['repos'])} | {fn(a['actions']['ADOPT'])} |")
# vendor
L+=["","## Vendored code: decisions","","| Code | App | Lines | Recommendation |","|---|---|---:|---|"]
for r in N:
    d=json.load(open(f'{S}/md/{r}-rightkit-files.json'))['files']; g=collections.OrderedDict()
    for f in d:
        if f['action'] in('VENDOR-OWN','VENDOR-KEEP') or (f['action']=='DROP' and 'vendor' in f['path'].lower()):
            k=(f['origTarget'],f['action'],f['capability']); g[k]=g.get(k,0)+f['loc']
    for (t,a,c),n in g.items(): L.append(f"| {c} | {N[r]} | {fn(n)} | {a}: `{t}` |")
L+=["","## Merges made across apps","","| Merged away | Into | Why |","|---|---|---|"]+[f"| `{k}` | `{v[0]}` | {v[1]} |" for k,v in ALIAS.items()]
L+=["","## What stays in apps","","Each app keeps only entry points (Tauri `main`/`lib`, iOS/Android app entry, engine binary & command table, CLI definitions), app manifests and release config. Largest leftovers are CodeRight's CLI (`main_runtime_*`, `cli_definitions*`), Legion's CLI commands, Pulse's per-platform entries (Windows `main.rs`, Mac `AppDelegate`, hub) and HeardRight's `app_run.rs`. Each app's mapping file lists them.",""]
open(S+'/md/master.md','w').write('\n'.join(L)+'\n'); print(len(new),len(crates),len(npm),len(sw),len(ext),len(ad))
