"""Read-only manifest/reference census. No build, install, registry, or graph claim."""
from pathlib import Path
import csv, hashlib, json, os, re, subprocess, tomllib
from datetime import datetime, timezone

HERE = Path(__file__).resolve().parent
RK = HERE.parents[3]
BASE = RK.parent
SKIP = {'.git', '.audit', '.blueprint', '.cache', '.legion-cache', 'node_modules',
        'target', '.build', 'dist', 'build', 'vendor', 'vendored', '.venv',
        'reports', 'audits', 'evidence', 'artifacts', '.next', 'coverage',
        'repos', 'archive', 'archives', 'snapshots', 'release-evidence'}
EXTS = {'.rs', '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs', '.swift', '.cs',
        '.kt', '.kts', '.py', '.sh', '.ps1', '.yml', '.yaml', '.pbxproj'}
ROOTS = {n: BASE/n for n in ['coderight', 'heardright', 'viewright', 'mailright',
    'scraperight', 'cutright', 'genright', 'voiceright', 'workright', 'pulse',
    'swiftright', 'legion', 'rustright', 'sellright', 'rightsites', 'postright', 'xtool']}
ROOTS.update({'lightcraft': BASE/'workflows/lightcraft', 'freecad': BASE/'workflows/freecad'})
ROOTS.update({f'membrane:{p.name}': p for p in Path('/Volumes/D/worktrees').glob('membrane-*') if p.is_dir()})
errors, hashes, edges, refs, repo_states = [], {}, [], [], []

def read(p):
    try:
        b = p.read_bytes()
        hashes[str(p)] = hashlib.sha256(b).hexdigest()
        return b.decode('utf-8')
    except (UnicodeError, OSError) as e:
        errors.append({'path': str(p), 'error': type(e).__name__})
        return None

def files(root):
    for base, dirs, names in os.walk(root):
        dirs[:] = [d for d in dirs if d not in SKIP and not d.startswith('.')]
        for n in names:
            yield Path(base)/n

def git(root, *args):
    return subprocess.run(['git', '-C', str(root), *args], capture_output=True, text=True).stdout.strip()

def lineof(s, name, section, ecosystem):
    current=''
    for i,l in enumerate(s.splitlines(),1):
        if ecosystem=='npm':
            m=re.match(r'\s*"(dependencies|devDependencies|peerDependencies|optionalDependencies)"\s*:\s*\{',l)
            if m:current=m[1]
            if current==section and re.match(r'\s*"'+re.escape(name)+r'"\s*:',l):return i
        else:
            if l.strip().startswith('['):
                try:
                    d=tomllib.loads(l+'\n__audit_marker=1\n');path=[]
                    while '__audit_marker' not in d:
                        k=next(iter(d));path.append(k);d=d[k]
                    current='.'.join(path)
                except (ValueError,TypeError):current=''
            if current==section and re.match(r'\s*[\"\']?'+re.escape(name)+r'[\"\']?\s*=',l):return i
    return 1

def cargo_tables(d, prefix=''):
    for k in ['dependencies','dev-dependencies','build-dependencies']:
        if isinstance(d.get(k),dict):
            yield prefix+k,d[k]
    for target, td in d.get('target',{}).items():
        yield from cargo_tables(td, prefix+f'target.{target}.')
    if isinstance(d.get('workspace'),dict):
        yield from cargo_tables(d['workspace'], prefix+'workspace.')

for app, root in ROOTS.items():
    repo_states.append({'app':app,'root':str(root),'exists':root.exists(),
        'head':git(root,'rev-parse','HEAD'),'status':git(root,'status','--porcelain')})
    for p in files(root):
        if p.name not in {'package.json','Cargo.toml','Package.swift','Package.resolved'} and p.suffix not in EXTS:
            continue
        s=read(p)
        if s is None: continue
        try:
            if p.name=='package.json':
                d=json.loads(s)
                for sec in ['dependencies','devDependencies','peerDependencies','optionalDependencies']:
                    for name,spec in d.get(sec,{}).items():
                        if name.startswith('@rightkit/'):
                            edges.append({'app':app,'manifest':str(p),'line':lineof(s,name,sec,'npm'),'ecosystem':'npm',
                                'section':sec,'dependency':name,'spec':spec,'features':[]})
            elif p.name=='Cargo.toml':
                d=tomllib.loads(s)
                for sec, deps in cargo_tables(d):
                    for key,spec in deps.items():
                        name=spec.get('package',key) if isinstance(spec,dict) else key
                        if name.startswith('rightkit-') or name=='agent-room-core':
                            edges.append({'app':app,'manifest':str(p),'line':lineof(s,key,sec,'cargo'),'ecosystem':'cargo',
                                'section':sec,'dependency':name,'spec':spec,'features':spec.get('features',[]) if isinstance(spec,dict) else []})
        except (ValueError, TypeError) as e:
            errors.append({'path':str(p),'error':str(e)})
        if p.suffix in EXTS:
            for i,line in enumerate(s.splitlines(),1):
                tokens=set(re.findall(r'@rightkit/[a-z0-9-]+|\brightkit_[a-z0-9_]+\b|\bRightKit[A-Za-z0-9_]*\b',line))
                for token in sorted(tokens):
                    refs.append({'app':app,'path':str(p),'line':i,'token':token})

summary={'generated_utc':datetime.now(timezone.utc).isoformat(), 'kind':'static-manifest-and-text-reference-census',
    'exclusions':sorted(SKIP),'roots':repo_states,'manifest_edges':edges,'references':refs,'errors':errors,
    'notes':['Declarations include workspace, test & build scopes; not all are runtime adoption.',
             'References are lexical occurrences including comments; source review decides actual adoption.',
             'No registry resolution, compiler, build, dynamic import evaluation, or graph completeness is claimed.']}
(HERE/'app-manifest-inventory.json').write_text(json.dumps(summary,indent=2)+'\n')
(HERE/'source-hashes.json').write_text(json.dumps(hashes,indent=2)+'\n')
with (HERE/'app-rightkit-dependencies.csv').open('w') as f:
    cols=['app','manifest','line','ecosystem','section','dependency','spec','features']
    w=csv.DictWriter(f,fieldnames=cols);w.writeheader()
    for e in edges: w.writerow({**e,'spec':json.dumps(e['spec']),'features':json.dumps(e['features'])})
with (HERE/'app-rightkit-references.csv').open('w') as f:
    w=csv.DictWriter(f,fieldnames=['app','path','line','token']);w.writeheader();w.writerows(refs)
print(json.dumps({'roots':len(ROOTS),'manifest_edges':len(edges),'references':len(refs),'hashed_files':len(hashes),'errors':len(errors)}))
