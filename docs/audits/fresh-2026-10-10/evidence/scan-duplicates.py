"""Cross-component exact normalized-line candidates; not a semantic clone verdict."""
from pathlib import Path
from collections import defaultdict
import hashlib, json, re

OUT=Path(__file__).resolve().parent
ROOT=OUT.parents[3]
SKIP={'node_modules','target','dist','.build','vendor','tests','test','benches',
      'fixtures','examples','example-app','.audit','.cache','artifacts','evidence',
      'preview','.preview','snapshots'}
blocks=defaultdict(list); files=0; significant=0
for folder in ['crates','packages','swift/RightKitSwift/Sources']:
    for p in (ROOT/folder).rglob('*'):
        rel=p.relative_to(ROOT)
        if not p.is_file() or any(part in SKIP for part in rel.parts):continue
        if p.suffix not in {'.rs','.ts','.tsx','.js','.mjs','.swift'}:continue
        if any(mark in p.name for mark in ['.test.','.spec.','.d.ts']):continue
        lines=[]
        for n,l in enumerate(p.read_text(errors='replace').splitlines(),1):
            s=re.sub(r'\s+',' ',l.strip())
            if not s or s.startswith(('//','/*','*','*/')):continue
            lines.append((n,s))
        owner='/'.join(rel.parts[:2]) if rel.parts[0]!='swift' else '/'.join(rel.parts[:4])
        files+=1;significant+=len(lines)
        for i in range(max(0,len(lines)-15)):
            chunk='\n'.join(x[1] for x in lines[i:i+16])
            if len(chunk)<400:continue
            digest=hashlib.sha256(chunk.encode()).hexdigest()
            blocks[digest].append({'owner':owner,'path':str(p),'start':lines[i][0],'end':lines[i+15][0]})
pairs=defaultdict(list)
for digest,hits in blocks.items():
    owners=sorted(set(h['owner'] for h in hits))
    if len(owners)<2:continue
    for i,a in enumerate(owners):
        for b in owners[i+1:]:
            key=(a,b);left=next(h for h in hits if h['owner']==a);right=next(h for h in hits if h['owner']==b)
            pairs[key].append({'sha256':digest,'left':left,'right':right})
summary=[]
for key,hits in sorted(pairs.items(),key=lambda kv:-len(kv[1])):
    distinct=[]
    for h in hits:
        if not any(x['left']['path']==h['left']['path'] and abs(x['left']['start']-h['left']['start'])<16 for x in distinct):
            distinct.append(h)
    summary.append({'components':key,'overlapping_window_count':len(hits),'representative_blocks':distinct[:12]})
result={'method':'Exact 16-line normalized first-party source windows >=400 characters; comment-only/blank lines removed; cross-component only. Candidates require semantic review. No AST or complete clone-graph claim.',
        'files':files,'significant_lines':significant,'window_hashes':len(blocks),'component_pairs':summary,'excluded_directory_names':sorted(SKIP)}
(OUT/'duplicate-candidates.json').write_text(json.dumps(result,indent=2)+'\n')
for x in summary[:30]:print(x['components'],x['overlapping_window_count'],[(h['left']['path'].replace(str(ROOT)+'/','')+':'+str(h['left']['start']),h['right']['path'].replace(str(ROOT)+'/','')+':'+str(h['right']['start'])) for h in x['representative_blocks'][:2]])
print('FILES',files,'LINES',significant,'PAIRS',len(summary))
