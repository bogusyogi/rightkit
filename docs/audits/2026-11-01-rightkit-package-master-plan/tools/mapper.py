#!/usr/bin/env python3
"""mapper.py <repo> <rules.tsv> <outdir>
rules.tsv: regex<TAB>target<TAB>action<TAB>capability  (first match wins; '#' comments)
actions: ADOPT (existing RightKit pkg covers it; delete app copy) | EXTEND (existing pkg gains it)
         NEW (new RightKit pkg) | SHELL (thin app composition that stays) | DROP (delete/replace w/ nothing)
         VENDOR-OWN (vendored 3rd-party: rewrite/own in RightKit) | VENDOR-KEEP (keep as plain dependency)
"""
import sys,re,subprocess,json,collections,os
repo,rules_path,out=sys.argv[1:4]
root=f"/home/user/{repo}"
CODE=re.compile(r'\.(rs|ts|tsx|js|mjs|cjs|jsx|swift|kt|kts|java|py|go|c|cc|cpp|h|hpp|m|mm|sh|ps1|css|scss|html|vue|svelte|sql|toml|gradle|metal|wgsl|glsl)$')
files=[f for f in subprocess.check_output(['git','-C',root,'ls-files','-z']).decode().split('\0') if f and CODE.search(f)]
rules=[]
import itertools
for ln in itertools.chain(open(os.path.dirname(rules_path)+"/_common.tsv"),open(rules_path)):
    if not ln.strip() or ln.startswith('#'): continue
    p=ln.rstrip('\n').split('\t')
    while len(p)<4: p.append('')
    rules.append((re.compile(p[0]),p[1],p[2],p[3]))
def loc(f):
    try: return sum(1 for _ in open(os.path.join(root,f),'rb'))
    except: return 0
rows=collections.OrderedDict(); out_files=[]; miss=[]
for f in files:
    for i,(rx,t,a,c) in enumerate(rules):
        if rx.search(f):
            n=loc(f); k=(t,a,c)
            r=rows.setdefault(k,{'target':t,'action':a,'capability':c,'files':0,'loc':0,'paths':[]})
            r['files']+=1; r['loc']+=n; r['paths'].append(f)
            out_files.append({'path':f,'loc':n,'target':t,'action':a,'capability':c}); break
    else: miss.append(f)
if miss:
    print("UNMAPPED",len(miss)); print("\n".join(miss[:80])); sys.exit(1)
os.makedirs(out,exist_ok=True)
json.dump({'repo':repo,'files':out_files},open(f"{out}/{repo}-files.json",'w'),indent=1)
json.dump(list(rows.values()),open(f"{out}/{repo}-rows.json",'w'),indent=1)
tot=collections.Counter(); totl=collections.Counter()
for r in rows.values(): tot[r['action']]+=r['files']; totl[r['action']]+=r['loc']
print(f"{repo}: {len(files)} code files, {sum(totl.values())} LOC, {len(rows)} rows")
for a in tot: print(f"  {a:12} {tot[a]:5} files {totl[a]:8} LOC")
