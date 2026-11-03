import fs from "node:fs"; import path from "node:path"; import {execFileSync} from "node:child_process";
const root="/home/user/rightkit"; const S=process.argv[2]; const units=process.argv.slice(3);
const recipes=JSON.parse(fs.readFileSync(root+"/ci/consumer-recipes.json","utf8"));
const out={};
for (const u of units) {
  const name="@rightkit/"+u; const e=recipes.npm[name];
  const pkg=JSON.parse(fs.readFileSync(`${root}/packages/${u}/package.json`,"utf8"));
  const consumer=JSON.parse(fs.readFileSync(`${root}/${path.dirname(e.id)}/package.json`,"utf8"));
  const deps={...consumer.dependencies};
  for (const d of e.closure.declaredOnly) if (d.startsWith("@rightkit/")) { const v=execFileSync("npm",["view",d,"version"],{encoding:"utf8"}).trim(); deps[d]="^"+v; }
  const dir=`${S}/csim-${u}`; fs.rmSync(dir,{recursive:true,force:true}); fs.mkdirSync(dir,{recursive:true});
  fs.writeFileSync(dir+"/package.json",JSON.stringify({name:"csim",private:true,dependencies:deps}));
  const pins=Object.entries(e.closure.install).filter(([k])=>k.startsWith("@rightkit/"));
  fs.writeFileSync(dir+"/pnpm-workspace.yaml",`packages: []\nminimumReleaseAgeExclude:\n  - "@rightkit/*"\noverrides:\n${pins.map(([k,v])=>`  "${k}": "${v}"\n`).join("")}`);
  try { execFileSync("corepack",["pnpm","install","--lockfile-only","--ignore-scripts","--no-frozen-lockfile"],{cwd:dir,env:{...process.env,COREPACK_ENABLE_DOWNLOAD_PROMPT:"0"},stdio:"pipe"}); } catch(err){ out[name]={error:String(err.stderr||err).slice(0,400)}; continue; }
  const lock=fs.readFileSync(dir+"/pnpm-lock.yaml","utf8"); const res={};
  for (const line of lock.split(/\r?\n/)) { const m=line.match(/^ {2}'?(@rightkit\/[a-z0-9-]+)@(.+?)'?:\s*$/); if(m){ (res[m[1]]??=new Set()).add(m[2].replace(/\(.*$/,"")); } }
  out[name]=Object.fromEntries(Object.entries(res).map(([k,v])=>[k,[...v]]));
  fs.rmSync(dir,{recursive:true,force:true});
}
fs.writeFileSync(S+"/closure-sim.json",JSON.stringify(out,null,1)); console.log(JSON.stringify(out).slice(0,1800));
