"""Read-only filesystem/manifest census. Not a symbol graph or integration proof."""
from pathlib import Path
import os, json, subprocess, hashlib, re, tomllib, csv, datetime
E=Path(__file__).resolve().parent
RK=E.parents[3]
SKIP={'.git','node_modules','target','.build','dist','build','out','.cache','.audit','.legion-cache','vendor','third_party','third-party','.pnpm','Caches','Library','models','model_registry','transcripts','memory-mirror','reports','docs/audits'}
ROOTS=[Path('/Volumes/D/claude'),Path('/Volumes/D/worktrees'),Path('/Users/adrdsouza/.codex/worktrees'),Path('/Users/adrdsouza/Documents'),Path('/Users/adrdsouza/Projects'),Path('/Users/adrdsouza/Developer')]
def git(root,*args):
    try:return subprocess.check_output(['git','-C',str(root),*args],stderr=subprocess.DEVNULL,timeout=15).decode(errors='replace').strip()
    except Exception:return ''
found={}
for base in ROOTS:
    if not base.exists():continue
    for here,dirs,files in os.walk(base):
        p=Path(here);depth=len(p.relative_to(base).parts)
        if '.git' in dirs or '.git' in files:
            found[str(p)]={'path':str(p),'head':git(p,'rev-parse','HEAD'),'origin':git(p,'remote','get-url','origin'),'git_marker':'directory' if '.git' in dirs else 'file'}
        dirs[:]=[d for d in dirs if d not in SKIP and not d.startswith('.') and depth<4]
        if base.name=='Documents':dirs[:]=[d for d in dirs if not d.startswith('Cockpit-')]
(E/'machine-repositories.json').write_text(json.dumps(list(found.values()),indent=2)+'\n')

# All primary named app/tool repositories plus retained product worktrees & workflows.
names=['coderight','heardright','viewright','mailright','scraperight','cutright','genright','voiceright','workright','pulse','swiftright','ember','legion','rustright','sellright','rightsites','xtool','postright']
scopes=[(n,Path('/Volumes/D/claude')/n) for n in names]
for base,prefix in [(Path('/Volumes/D/worktrees'),'worktree:'),(Path('/Volumes/D/claude/workflows'),'workflow:')]:
    if base.exists():scopes.extend((prefix+p.name,p) for p in base.iterdir() if p.is_dir())
archive=Path('/Volumes/D/claude/archive')
if archive.exists():scopes.extend(('archive:'+p.name,p) for p in archive.iterdir() if p.is_dir() and ('right' in p.name.lower() or 'citadel' in p.name.lower()))
scopes.insert(0,('rightkit',RK))
excluded={'audit','audits','vendor','third_party','third-party','node_modules','target','.build','dist','build','out','.cache','.audit','.legion-cache','.git','.pnpm','reports','model_registry'}
source_ext={'.rs','.toml','.json','.yaml','.yml','.ts','.tsx','.js','.mjs','.cjs','.swift','.csproj','.py','.go','.mod','.sh','.ps1','.html','.css','.pbxproj','.entitlements','.xcprivacy'}
manifests=[];edges=[];refs=[];hashes={};roots=[];errors=[]
for name,root in scopes:
    rec={'app':name,'root':str(root),'exists':root.exists(),'head':git(root,'rev-parse','HEAD') if root.exists() else ''};roots.append(rec)
    if not root.exists():continue
    data=git(root,'ls-files','--cached','--others','--exclude-standard','-z')
    files=sorted(set(data.split('\0'))) if data else [str(p.relative_to(root)) for p in root.rglob('*') if p.is_file()]
    for rel in files:
        parts=Path(rel).parts
        if any(x in excluded for x in parts) or rel.startswith('spikes/rk2-') or rel.startswith('contracts-pending/'):continue
        p=root/rel
        if not p.is_file() or p.is_symlink() or p.stat().st_size>5_000_000:continue
        if p.suffix not in source_ext and p.name not in ['Cargo.lock','Package.resolved','VERSION']:continue
        try:b=p.read_bytes();s=b.decode('utf-8')
        except Exception:continue
        hashes[str(p)]=hashlib.sha256(b).hexdigest()
        if p.name=='package.json':
            try:d=json.loads(s)
            except Exception as ex:errors.append({'path':str(p),'error':str(ex)});continue
            if not d.get('name'):continue
            manifests.append({'app':name,'path':str(p),'ecosystem':'npm','name':d['name'],'version':d.get('version'),'private':d.get('private',False),'manifest':d})
            for section in ['dependencies','devDependencies','peerDependencies','optionalDependencies']:
                for dep,spec in d.get(section,{}).items():
                    line=next((i for i,l in enumerate(s.splitlines(),1) if '"'+dep+'"' in l),1)
                    edges.append({'app':name,'manifest':str(p),'ecosystem':'npm','section':section,'dependency':dep,'spec':spec,'line':line})
        elif p.name=='Cargo.toml':
            try:d=tomllib.loads(s)
            except Exception as ex:errors.append({'path':str(p),'error':str(ex)});continue
            pkg=d.get('package',{})
            if pkg:manifests.append({'app':name,'path':str(p),'ecosystem':'cargo','name':pkg.get('name'),'version':pkg.get('version'),'publish':pkg.get('publish',True),'manifest':d})
            sections=[(k,v) for k,v in d.items() if k in ['dependencies','dev-dependencies','build-dependencies']]
            sections += [('workspace.dependencies',d.get('workspace',{}).get('dependencies',{}))]
            for target,td in d.get('target',{}).items():sections += [(f'target.{target}.{k}',v) for k,v in td.items() if k in ['dependencies','dev-dependencies','build-dependencies']]
            for section,deps in sections:
                for dep,spec in deps.items():
                    actual=spec.get('package',dep) if isinstance(spec,dict) else dep
                    line=next((i for i,l in enumerate(s.splitlines(),1) if re.match(r'\s*'+re.escape(dep)+r'\s*=',l)),1)
                    edges.append({'app':name,'manifest':str(p),'ecosystem':'cargo','section':section,'dependency':actual,'alias':dep,'spec':spec,'line':line})
        elif p.name=='Package.swift':manifests.append({'app':name,'path':str(p),'ecosystem':'swift','name':re.search(r'name:\s*"([^"]+)"',s).group(1) if re.search(r'name:\s*"([^"]+)"',s) else p.parent.name,'source':s})
        elif p.suffix=='.csproj' or p.name in ['go.mod','pyproject.toml','requirements.txt']:manifests.append({'app':name,'path':str(p),'ecosystem':'other','name':p.parent.name,'source':s})
        if name!='rightkit':
            for i,line in enumerate(s.splitlines(),1):
                if any(t in line for t in ['@rightkit/','rightkit_','rightkit-','RightKit']):refs.append({'app':name,'path':str(p),'line':i,'text':line[:700]})
inventory={'captured_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'method':'Filesystem/manifest census; text references are lexical, not integration proof. Excludes generated, vendor, caches, prior audits & binaries.','roots':roots,'manifests':manifests,'dependency_edges':edges,'references':refs,'errors':errors}
(E/'current-inventory.json').write_text(json.dumps(inventory,indent=2,default=str)+'\n')
(E/'source-hashes.json').write_text(json.dumps(hashes,indent=2)+'\n')
for filename,rows,fields in [('rightkit-dependencies.csv',[x for x in edges if x['app']=='rightkit'],['app','manifest','ecosystem','section','dependency','spec','line']),('app-rightkit-dependencies.csv',[x for x in edges if x['app']!='rightkit' and (x['dependency'].startswith(('@rightkit/','rightkit-')) or x['dependency']=='agent-room-core')],['app','manifest','ecosystem','section','dependency','spec','line'])]:
    with (E/filename).open('w') as f:
        w=csv.DictWriter(f,fieldnames=fields,extrasaction='ignore');w.writeheader();w.writerows([{**r,'spec':json.dumps(r['spec'])} for r in rows])
print(json.dumps({'repositories':len(found),'roots':len(roots),'manifests':len(manifests),'dependency_edges':len(edges),'references':len(refs),'hashed_files':len(hashes),'errors':errors},indent=2))
