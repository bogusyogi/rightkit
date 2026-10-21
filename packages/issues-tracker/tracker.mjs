// Builds the issues.html model and page for one repository. Pure functions: the CLI does the I/O.

const DAY = 86400000;

/** Status chips, highest priority first. Labels set them; nothing is guessed. */
export const STATUS_LABELS = [
  ["decision", ["needs-decision", "needs-adrian"]],
  ["live", ["needs-live-run", "needs-live-check"]],
  ["fixed", ["built-needs-check", "fixed-unverified"]],
  ["blocked", ["blocked", "blocked-upstream"]],
  ["tracking", ["tracking"]],
];

/** Label prefixes that name a group, highest priority first, e.g. `group:Hub` beats `area:Notch`. */
export const GROUP_LABEL_PREFIXES = ["group:", "component:", "feature:", "area:"];

const cleanName = (value) => String(value ?? "").trim().replace(/\s+/g, " ");
const byCodePoint = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

const labelNames = (issue) => (issue.labels || []).map((l) => (typeof l === "string" ? l : l.name));

export function statusOf(issue) {
  if (String(issue.state).toUpperCase() === "CLOSED") return "done";
  const names = new Set(labelNames(issue).map((l) => String(l).toLowerCase()));
  for (const [status, labels] of STATUS_LABELS) if (labels.some((l) => names.has(l))) return status;
  return "open";
}

/** Normalises the optional `.rightkit/issue-groups.json`: `{ groups?, issues?, rules? }`. */
export function normaliseGroups(file) {
  const f = file || {};
  // Names are trimmed and de-duplicated case-insensitively; the first declared spelling wins.
  const canon = new Map();
  for (const name of Array.isArray(f.groups) ? f.groups : []) {
    const clean = cleanName(name);
    if (clean && !canon.has(clean.toLowerCase())) canon.set(clean.toLowerCase(), clean);
  }
  const groups = [...canon.values()];
  const issues = f.issues && typeof f.issues === "object" ? f.issues : {};
  const rules = Array.isArray(f.rules)
    ? f.rules.map((rule) => {
        const [name, patterns] = Array.isArray(rule) ? rule : [];
        if (!name || !Array.isArray(patterns)) throw new Error(`issue-groups rule must be ["Group", ["regex", ...]]: ${JSON.stringify(rule)}`);
        return [String(name), patterns.map((p) => {
          try {
            return new RegExp(p, "i");
          } catch (error) {
            throw new Error(`issue-groups rule "${name}" has an invalid pattern ${JSON.stringify(p)}: ${error.message}`);
          }
        })];
      })
    : [];
  return { groups, canon, issues, rules };
}

/**
 * Explicit issue mapping, then a group label, then title rules, else "Other".
 * Several group labels resolve by prefix priority (group: > component: > feature: > area:),
 * then by sorted label name, so label order never changes the group.
 */
export function groupOf(issue, groups) {
  const canonical = (name) => {
    const clean = cleanName(name);
    return groups.canon?.get(clean.toLowerCase()) ?? clean;
  };
  const explicit = Object.hasOwn(groups.issues, String(issue.number)) ? cleanName(groups.issues[String(issue.number)]) : "";
  if (explicit) return canonical(explicit);
  const candidates = [];
  for (const name of labelNames(issue)) {
    const lower = String(name).toLowerCase();
    const rank = GROUP_LABEL_PREFIXES.findIndex((p) => lower.startsWith(p));
    const value = rank < 0 ? "" : cleanName(String(name).slice(GROUP_LABEL_PREFIXES[rank].length));
    if (value) candidates.push({ rank, name: lower, value });
  }
  candidates.sort((x, y) => x.rank - y.rank || byCodePoint(x.name, y.name) || byCodePoint(x.value, y.value));
  if (candidates.length) return canonical(candidates[0].value);
  for (const [name, patterns] of groups.rules) if (patterns.some((r) => r.test(issue.title || ""))) return canonical(name);
  return "Other";
}

/**
 * Maps GitHub REST issues (`GET /repos/{o}/{r}/issues`) to the model's shape and drops pull
 * requests. Input already in the model's shape passes through. A duplicate number is an error.
 */
export function fromRestIssues(raw) {
  if (!Array.isArray(raw)) throw new Error("issues must be a JSON array of issues");
  const seen = new Set();
  const out = [];
  for (const item of raw) {
    if (!item || typeof item !== "object") throw new Error("issues must be objects");
    if (item.pull_request) continue;
    const number = Number(item.number);
    if (!Number.isInteger(number) || number < 1) throw new Error(`issue without a valid number: ${JSON.stringify(item.title ?? null)}`);
    if (seen.has(number)) throw new Error(`issue #${number} appears twice in the inventory`);
    seen.add(number);
    out.push({
      number,
      title: item.title,
      state: String(item.state || "").toUpperCase(),
      labels: (item.labels || []).map((l) => (typeof l === "string" ? l : l.name)),
      updatedAt: item.updatedAt ?? item.updated_at ?? null,
      closedAt: item.closedAt ?? item.closed_at ?? null,
      url: item.html_url ?? item.url,
      body: item.body ?? "",
    });
  }
  return out;
}

/** First paragraph of the body, plain and short: the question shown in the needs section. */
export function noteOf(body) {
  const text = String(body || "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .split(/\r?\n\s*\r?\n/)
    .map((p) => p.replace(/\s+/g, " ").trim())
    .find((p) => p && !p.startsWith("#"));
  if (!text) return "";
  return text.length > 280 ? `${text.slice(0, 277)}...` : text;
}

/**
 * @param {object} input
 * @param {Array} input.issues the complete inventory, open and closed, in the model's shape (see fromRestIssues)
 * @param {object} [input.groupsFile] parsed `.rightkit/issue-groups.json`
 * @param {string} input.repo owner/name
 * @param {Date} [input.now]
 * @param {number} [input.closedDays] closed issues are "recent" (default view) for this many days (default 14); every closed issue stays in the data
 */
export function buildModel({ issues, groupsFile, repo, now = new Date(), closedDays = 14 }) {
  if (!Array.isArray(issues)) throw new Error("issues must be a JSON array of issues");
  const groups = normaliseGroups(groupsFile);
  const kept = [];
  for (const issue of issues) {
    const status = statusOf(issue);
    // A closed issue without a usable closedAt ages from its last update.
    const closedAt = status === "done" ? issue.closedAt || issue.updatedAt || null : null;
    const age = closedAt ? now - new Date(closedAt) : NaN;
    kept.push({
      n: Number(issue.number),
      title: String(issue.title || ""),
      url: safeUrl(issue.url),
      status,
      group: groupOf(issue, groups),
      updated: issue.updatedAt || null,
      closed: closedAt,
      recent: status !== "done" || age <= closedDays * DAY,
      note: status === "decision" || status === "live" ? noteOf(issue.body) : "",
    });
  }
  // Names that differ only by case collapse to the declared spelling, else the smallest spelling.
  const spelling = new Map(groups.canon);
  for (const name of [...new Set(kept.map((i) => i.group))].sort(byCodePoint)) if (!spelling.has(name.toLowerCase())) spelling.set(name.toLowerCase(), name);
  for (const i of kept) i.group = spelling.get(i.group.toLowerCase());
  const open = kept.filter((i) => i.status !== "done");
  const count = (f) => open.filter(f).length;
  const stats = {
    open: open.length,
    needs: count((i) => i.status === "decision" || i.status === "live"),
    decisions: count((i) => i.status === "decision"),
    live: count((i) => i.status === "live"),
    built: count((i) => i.status === "fixed"),
    blocked: count((i) => i.status === "blocked"),
    closedWeek: kept.filter((i) => i.status === "done" && i.closed && now - new Date(i.closed) <= 7 * DAY).length,
    closedTotal: kept.length - open.length,
  };
  const declared = new Set(groups.groups);
  const seen = [...new Set(kept.map((i) => i.group))];
  const names = [...groups.groups, ...seen.filter((g) => !declared.has(g)).sort((a, b) => (a === "Other") - (b === "Other") || a.localeCompare(b))];
  return { generatedAt: now.toISOString(), repo, closedDays, stats, groups: names, issues: kept };
}

/** Only http(s) links reach an href; anything else (javascript:, data:) becomes no link. */
export function safeUrl(url) {
  return /^https?:\/\//i.test(String(url || "")) ? String(url) : "#";
}

const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

const STYLE = String.raw`:root{color-scheme:light dark;--bg:#f6f7f9;--card:#fff;--fg:#1b1f24;--mute:#667085;--line:#e3e6eb;--accent:#2f6fed;--on-accent:#fff;
--decision:#b54708;--decision-bg:#fef0c7;--live:#175cd3;--live-bg:#d1e9ff;--fixed:#067647;--fixed-bg:#d3f8df;
--blocked:#b42318;--blocked-bg:#fee4e2;--tracking:#475467;--tracking-bg:#eaecf0;--open:#344054;--open-bg:#f2f4f7;--done:#067647;--done-bg:#e6f6ec}
@media (prefers-color-scheme:dark){:root{--bg:#0f1216;--card:#171b21;--fg:#e6e9ee;--mute:#98a2b3;--line:#2a3039;--accent:#7aa2ff;--on-accent:#0f1216;
--decision:#fec84b;--decision-bg:#4a3507;--live:#84caff;--live-bg:#0f2f55;--fixed:#75e0a7;--fixed-bg:#0c3a23;
--blocked:#fda29b;--blocked-bg:#4a1512;--tracking:#c0c6d0;--tracking-bg:#2a3039;--open:#d0d5dd;--open-bg:#232830;--done:#75e0a7;--done-bg:#0c3a23}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif}
main{max-width:1000px;margin:0 auto;padding:16px}
h1{font-size:22px;margin:4px 0 2px}h2{font-size:17px;margin:24px 0 8px}
.sub{color:var(--mute);font-size:13px}.sub code{background:var(--open-bg);padding:1px 5px;border-radius:4px}
.strip{display:grid;grid-template-columns:repeat(auto-fit,minmax(130px,1fr));gap:8px;margin:14px 0}
.stat{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:10px 12px}
.stat b{display:block;font-size:26px;line-height:1.1}.stat span{font-size:12px;color:var(--mute)}
.stat.hot b{color:var(--decision)}
.bar{position:sticky;top:0;background:var(--bg);padding:8px 0;z-index:5;border-bottom:1px solid var(--line)}
input[type=search]{width:100%;padding:9px 12px;font-size:15px;border:1px solid var(--line);border-radius:8px;background:var(--card);color:var(--fg)}
.chips{display:flex;flex-wrap:wrap;gap:6px;margin-top:8px}
.chip{border:1px solid var(--line);background:var(--card);color:var(--fg);border-radius:999px;padding:4px 11px;font-size:13px;cursor:pointer}
.chip[aria-pressed=true]{background:var(--accent);border-color:var(--accent);color:var(--on-accent)}
:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.need{background:var(--card);border:1px solid var(--line);border-left:4px solid var(--decision);border-radius:8px;padding:9px 12px;margin:6px 0}
.need.live{border-left-color:var(--live)}
.need a.n{font-weight:600}.need .q{margin-top:3px}.need .meta{font-size:12px;color:var(--mute)}
details.card{background:var(--card);border:1px solid var(--line);border-radius:10px;margin:8px 0}
summary{cursor:pointer;padding:11px 14px;display:flex;flex-wrap:wrap;gap:8px;align-items:center;font-weight:600;list-style:none}
summary::-webkit-details-marker{display:none}
summary::before{content:"\25B8";color:var(--mute)}details[open]>summary::before{content:"\25BE"}
summary .cnt{margin-left:auto;font-weight:400;font-size:12px;color:var(--mute)}
.row{display:grid;grid-template-columns:56px 1fr auto;gap:4px 10px;padding:8px 14px;border-top:1px solid var(--line);align-items:baseline}
.row a{color:var(--accent);text-decoration:none}.row a:hover{text-decoration:underline}
.row .t{min-width:0;overflow-wrap:anywhere}.row .u{font-size:12px;color:var(--mute);white-space:nowrap}
.st{display:inline-block;font-size:11px;font-weight:600;padding:2px 8px;border-radius:999px;white-space:nowrap}
.st.decision{color:var(--decision);background:var(--decision-bg)}.st.live{color:var(--live);background:var(--live-bg)}
.st.fixed{color:var(--fixed);background:var(--fixed-bg)}.st.blocked{color:var(--blocked);background:var(--blocked-bg)}
.st.tracking{color:var(--tracking);background:var(--tracking-bg)}.st.open{color:var(--open);background:var(--open-bg)}
.st.done{color:var(--done);background:var(--done-bg)}
.mini{display:inline-flex;gap:4px;flex-wrap:wrap}
.empty{color:var(--mute);padding:8px 0}
@media (max-width:560px){.row{grid-template-columns:48px 1fr}.row .u{grid-column:2}.row .st{grid-column:2;justify-self:start}}`;

// Client script. D (the model) and WHO (the person decisions go to) are defined before it.
const SCRIPT = String.raw`const LBL={decision:"Needs decision",live:"Live check",fixed:"Built, needs check",blocked:"Blocked",tracking:"Tracking",open:"Open",done:"Done"};
const PRI={decision:0,live:1,fixed:2,blocked:3,open:4,tracking:5,done:6};
const esc=s=>String(s).replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const ago=d=>{if(!d)return"";const s=(Date.now()-new Date(d))/1e3;if(s<3600)return Math.max(1,Math.round(s/60))+"m ago";if(s<86400)return Math.round(s/3600)+"h ago";return Math.round(s/86400)+"d ago"};
const S=D.stats,NEED="Needs "+WHO;
document.getElementById("gen").innerHTML="Generated "+esc(new Date(D.generatedAt).toLocaleString())+" from "+esc(D.repo)+" (all "+D.issues.length+" issues; the default view is open plus closed in the last "+D.closedDays+" days). Regenerate: <code>pnpm exec right-issues</code>";
document.getElementById("strip").innerHTML=[["Open",S.open,""],[NEED,S.needs,"hot"],["Built, needs check",S.built,""],["Blocked",S.blocked,""],["Closed this week",S.closedWeek,""]]
 .map(a=>'<div class="stat '+a[2]+'"><b>'+a[1]+'</b><span>'+esc(a[0])+(a[0]==NEED?' ('+S.decisions+' decisions, '+S.live+' live checks)':'')+'</span></div>').join("");
let filter="active",query="";
const chips=[["active","Open + recent"],["decision","Needs decision"],["live","Live check"],["fixed","Built"],["blocked","Blocked"],["tracking","Tracking"],["open","Open"],["done","Done"],["all","All"]];
const cEl=document.getElementById("chips");
function drawChips(){cEl.innerHTML=chips.map(c=>{const n=c[0]=="active"?D.issues.filter(i=>i.recent).length:c[0]=="all"?D.issues.length:D.issues.filter(i=>i.status==c[0]).length;return '<button type="button" class="chip" data-f="'+c[0]+'" aria-pressed="'+(filter==c[0])+'">'+c[1]+' '+n+'</button>'}).join("")}
cEl.onclick=e=>{const b=e.target.closest("button");if(!b)return;filter=b.dataset.f;render()};
document.getElementById("q").oninput=e=>{query=e.target.value.trim().toLowerCase();render()};
const match=i=>{if(filter=="active"&&!i.recent)return false;if(filter!="active"&&filter!="all"&&i.status!=filter)return false;if(!query)return true;
 return query.split(/\s+/).every(w=>(i.n+" "+i.title+" "+i.group+" "+i.note+" "+LBL[i.status]).toLowerCase().includes(w))};
function row(i){return '<div class="row"><a href="'+esc(i.url)+'">#'+i.n+'</a><span class="t">'+esc(i.title)+'</span><span class="u"><span class="st '+i.status+'">'+LBL[i.status]+'</span> '+(i.status=="done"?"closed "+ago(i.closed):ago(i.updated))+'</span></div>'}
function need(i){return '<div class="need '+i.status+'"><a class="n" href="'+esc(i.url)+'">#'+i.n+'</a> '+esc(i.title)+'<div class="q">'+(i.note?esc(i.note):'<span class="sub">See the issue for the question.</span>')+'</div><div class="meta">'+esc(i.group)+' | updated '+ago(i.updated)+'</div></div>'}
function render(){drawChips();const m=D.issues.filter(match);const nd=m.filter(i=>i.status=="decision"),nl=m.filter(i=>i.status=="live");
 let h='<h2>'+esc(NEED)+'</h2>';
 if(nd.length||nl.length){if(nd.length)h+='<div class="sub">Decisions ('+nd.length+')</div>'+nd.map(need).join("");if(nl.length)h+='<div class="sub" style="margin-top:10px">Live checks ('+nl.length+')</div>'+nl.map(need).join("")}
 else h+='<div class="empty">Nothing matches.</div>';
 document.getElementById("needs").innerHTML=h;
 const g=D.groups.map(name=>{const rows=m.filter(i=>i.group==name).sort((a,b)=>PRI[a.status]-PRI[b.status]||b.n-a.n);if(!rows.length)return"";
  const c={};rows.forEach(r=>c[r.status]=(c[r.status]||0)+1);
  const mini=Object.keys(c).sort((a,b)=>PRI[a]-PRI[b]).map(k=>'<span class="st '+k+'">'+c[k]+' '+LBL[k]+'</span>').join(" ");
  return '<details class="card"'+(query?" open":"")+'><summary>'+esc(name)+' <span class="mini">'+mini+'</span><span class="cnt">'+rows.length+' shown</span></summary>'+rows.map(row).join("")+'</details>'}).join("");
 document.getElementById("groups").innerHTML=g||'<div class="empty">No issues match.</div>'}
render();`;

/** Data embedded in a <script>: no `</script>`, no line separators. */
export function embedJson(value) {
  return JSON.stringify(value)
    .replace(/</g, "\\u003c")
    .replace(/\u2028/g, "\\u2028")
    .replace(/\u2029/g, "\\u2029");
}

export function renderHtml(model, { who = "Adrian" } = {}) {
  const name = model.repo.split("/").pop();
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>${escapeHtml(name)} issues</title>
<style>
${STYLE}
</style></head><body><main>
<h1>${escapeHtml(name)} issues</h1>
<div class="sub" id="gen"></div>
<noscript><p>This page needs JavaScript (inline, offline) to render the issue list.</p></noscript>
<div class="strip" id="strip"></div>
<div class="bar"><input type="search" id="q" placeholder="Search number, title, group, question" aria-label="Search issues"><div class="chips" id="chips"></div></div>
<div id="needs"></div>
<h2>Groups</h2>
<div id="groups"></div>
</main>
<script>
const D=${embedJson(model)};
const WHO=${embedJson(who)};
${SCRIPT}
</script></body></html>
`;
}
