"""Advance one visually reviewed draft; stores critique before launching next image."""
import argparse
import json
from pathlib import Path
import subprocess

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('previous_job',type=Path)
p.add_argument('key');p.add_argument('value',help='JSON value')
p.add_argument('--critique',required=True);p.add_argument('--blender',required=True)
a=p.parse_args();prior=json.loads(a.previous_job.read_text());base=a.previous_job.parent
image=Path(prior['output_dir'])/'lookdev.png'
if not image.is_file():p.error('previous image must exist; inspect it before advancing')
number=int(a.previous_job.stem.split('-')[-1]);target=base/f'job-{number+1:02}.json'
if target.exists():p.error('next iteration already exists')
log=base/'observations.json';observations=json.loads(log.read_text()) if log.exists() else []
value=json.loads(a.value)
observations.append(dict(iteration=number,image=str(image),viewed=True,critique=a.critique,next_change={a.key:value}))
log.write_text(json.dumps(observations,indent=2)+'\n')
prior['job_id']=f'editorial-{number+1:02}';prior['output_dir']=str(base/f'iter-{number+1:02}')
prior['params'].setdefault('lookdev_settings',{})[a.key]=value
target.write_text(json.dumps(prior,indent=2)+'\n')
root=Path(__file__).resolve().parents[1]
with (base/f'iter-{number+1:02}.log').open('w') as stream:
    result=subprocess.run([a.blender,'-b','--factory-startup','--python-exit-code','1','--python',str(root/'render_job.py'),'--',str(target)],cwd=root,stdout=stream,stderr=subprocess.STDOUT,timeout=120,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
print(json.dumps(dict(iteration=number+1,exit_code=result.returncode,image=str(Path(prior['output_dir'])/'lookdev.png'))))
raise SystemExit(result.returncode)
