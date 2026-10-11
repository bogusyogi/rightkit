from pathlib import Path
import json,hashlib,collections,datetime
OUT=Path(__file__).resolve().parent
catalog=json.loads((OUT/'catalog.json').read_text()); ids={r['id'] for r in catalog}; corpus=json.loads((OUT/'corpus.json').read_text())
errors=[]; warnings=[]; snapshots={}; apps=[]
for p in sorted((OUT/'apps').glob('*.json')):
 d=json.loads(p.read_text()); app=d['app']; apps.append(app); cells=d['cells']; got=[c['component_id'] for c in cells]
 if set(got)!=ids or len(got)!=len(ids):errors.append([app,'catalog mismatch',len(got),sorted(ids-set(got))])
 for c in cells:
  key=[app,c['component_id']]; status=c['status']
  if status not in ['Y','N','M','NA']:errors.append(key+['unresolved status',status])
  requirements={'Y':['shared_evidence'],'N':['local_evidence','owner_evidence'],'M':['shared_evidence','local_evidence','owner_evidence'],'NA':['applicability_evidence']}.get(status,[])
  for field in requirements:
   if not c.get(field):errors.append(key+['missing evidence',field])
  for field in ['shared_evidence','local_evidence','owner_evidence','applicability_evidence']:
   for e in c.get(field,[]):
    f=Path(e.get('path','')); line=e.get('line')
    if not f.is_absolute() or not f.is_file():errors.append(key+['missing file',str(f)]);continue
    try:
     blob=f.read_bytes(); lines=blob.decode().splitlines()
    except (UnicodeDecodeError,OSError):errors.append(key+['not text source',str(f)]);continue
    if not isinstance(line,int) or not 1<=line<=len(lines):errors.append(key+['invalid line',str(f),line,len(lines)]);continue
    if any(x in f.parts for x in ['node_modules','target','.build','dist','.cache','DerivedData']):errors.append(key+['forbidden generated evidence',str(f)])
    if field=='owner_evidence' and any(x in str(f) for x in ['.test.','/tests/']):errors.append(key+['test-only owner evidence',str(f)])
    if field=='shared_evidence' and f.name in ['Cargo.toml','Package.swift','package-lock.json','pnpm-lock.yaml','Cargo.lock']:warnings.append(key+['declaration shared proof',str(f)])
    if str(f) not in snapshots:snapshots[str(f)]={'sha256':hashlib.sha256(blob).hexdigest(),'line_count':len(lines),'anchors':{}}
    snapshots[str(f)]['anchors'][str(line)]='\n'.join(lines[max(0,line-3):min(len(lines),line+5)])
  if status in ['N','M'] and any(x in c.get('remaining_work','') for x in ['where semantics match','at its owner boundary','Route production callers through']):warnings.append(key+['generic migration task'])
  if status=='NA' and ('No active' in c['rationale'] and ('caller' in c['rationale'] or 'scope' in c['rationale'])):warnings.append(key+['NA absence needs semantic review'])
missing=[a['app'] for a in corpus['apps'] if not (OUT/'apps'/(a['slug']+'.json')).exists()]
baseline=json.loads((OUT/'evidence/source-baseline.json').read_text())['files']; flat={k:v for a in baseline.values() for k,v in a.items()}
drift=[p for p,v in snapshots.items() if p in flat and v['sha256']!=flat[p]]
result={'validated_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'apps_received':len(apps),'requested_apps':len(corpus['apps']),'rows_per_app':len(ids),'missing_apps':missing,'errors':errors,'warnings':warnings,'cited_files':len(snapshots),'cited_files_changed_since_baseline':drift,'complete':not errors and not missing}
result['validation_scope']='Structural completeness & citation integrity. Semantic judgments are documented in app reports & RECONCILIATION.md.'
result['warning_interpretation']='Text-pattern review prompts, retained for transparency; they are not automatic classification failures or a substitute for source review.'
(OUT/'evidence/validation.json').write_text(json.dumps(result,indent=2)+'\n');(OUT/'evidence/citation-snapshots.json').write_text(json.dumps(snapshots,indent=2)+'\n')
print(json.dumps({k:v for k,v in result.items() if k not in ['warnings','cited_files_changed_since_baseline']},indent=2));print('Warnings:',len(warnings),'Cited drift:',len(drift))
