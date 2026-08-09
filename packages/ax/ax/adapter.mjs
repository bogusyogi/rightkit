import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
const CAP=1048576;
export class RightAxAdapterError extends Error { constructor(code,details={}) { super(code); this.code=code; this.details=details; } }
const fail=(code,details)=>{throw new RightAxAdapterError(code,details)};
const text=(value)=>Buffer.concat(value).toString("utf8");
export async function runAdapter({adapter,request,timeoutMs=request?.limits?.timeoutMs}) {
  if (!Number.isInteger(timeoutMs)||timeoutMs<1) fail("adapter_protocol_invalid",{adapterId:adapter?.id,path:"$.limits.timeoutMs",reason:"invalid"});
  let child; try { child=spawn(adapter.command,adapter.args??[],{shell:false,detached:process.platform!=="win32",stdio:["pipe","pipe","pipe"],windowsHide:true}); } catch { fail("adapter_spawn_failed",{adapterId:adapter.id}); }
  const out=[],err=[]; let terminal=null, timer;
  const kill=()=>{ if(!child||child.exitCode!==null)return; if(process.platform==="win32")spawn("taskkill",["/PID",String(child.pid),"/T","/F"],{shell:false,windowsHide:true});else try{process.kill(-child.pid,"SIGKILL")}catch{child.kill("SIGKILL")} };
  const exited=new Promise((resolve,reject)=>{child.once("error",()=>reject(new RightAxAdapterError("adapter_spawn_failed",{adapterId:adapter.id})));child.once("exit",(code,signal)=>resolve({code,signal}));});
  const collect=(target,code)=>(chunk)=>{target.push(chunk);if(Buffer.concat(target).length>CAP&&!terminal){terminal=code;kill();}}; child.stdout.on("data",collect(out,"adapter_stdout_limit"));child.stderr.on("data",collect(err,"adapter_stderr_limit"));
  child.stdin.end(`${JSON.stringify(request)}\n`); const timeout=new Promise(resolve=>timer=setTimeout(()=>{if(child.exitCode!==null)return;if(!terminal)terminal="adapter_timeout";kill();resolve(null)},timeoutMs));
  const result=await Promise.race([exited,timeout]); clearTimeout(timer); if(terminal) fail(terminal,{adapterId:adapter.id,...(terminal==="adapter_timeout"?{timeoutMs}:{maxBytes:CAP})}); if(!result?.code&&!result?.signal){} else if(result.signal)fail("adapter_signaled",{adapterId:adapter.id,signal:result.signal,stderrExcerpt:text(err).slice(0,200)}); else if(result.code!==0)fail("adapter_exit_nonzero",{adapterId:adapter.id,exitCode:result.code,stderrExcerpt:text(err).slice(0,200)});
  const raw=text(out); if(!/^\s*\{[\s\S]*\}\s*$/.test(raw))fail("adapter_malformed_json",{adapterId:adapter.id}); let value;try{value=JSON.parse(raw)}catch{fail("adapter_malformed_json",{adapterId:adapter.id})};
  const bad=(path,reason)=>fail("adapter_protocol_invalid",{adapterId:adapter.id,path,reason}); if(!value||value.schemaVersion!==1)bad("$.schemaVersion","expected 1"); if(value.agentId!==adapter.id)fail("adapter_agent_mismatch",{adapterId:adapter.id,observedAgentId:value.agentId}); for(const key of ["operations","reasonCodes"])if(!Array.isArray(value[key])||value[key].some(x=>typeof x!=="string"))bad(`$.${key}`,"string array"); if(value.metrics?.toolCalls!==value.operations.length)bad("$.metrics.toolCalls","must equal operations length"); if(new Set(value.reasonCodes).size!==value.reasonCodes.length)bad("$.reasonCodes","unique"); for(const key of ["toolCalls","tokens","costUsd","durationMs"])if(!Number.isFinite(value.metrics?.[key]))bad(`$.metrics.${key}`,"finite"); const map={toolCalls:"maxToolCalls",tokens:"maxTokens",costUsd:"maxCostUsd",durationMs:"timeoutMs"};for(const [metric,limit]of Object.entries(map))if(value.metrics[metric]>request.limits[limit])fail("adapter_limit_exceeded",{adapterId:adapter.id,metric,observed:value.metrics[metric],limit:request.limits[limit]}); return value;
}
export function canary(caseId,artifactDigest){return `RIGHTAX_CANARY_${createHash("sha256").update(`${caseId}${artifactDigest}`).digest("hex")}`;}
