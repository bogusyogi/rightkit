"""Assemble this run's reviewed evidence only; never reads previous audits."""
from pathlib import Path
import json, re, csv, datetime

E = Path(__file__).resolve().parent
O = E.parent
RK = O.parents[2]
def link(label, path, line=None):
    return f'[{label}](<{path}' + (f':{line}' if line else '') + '>)'
def evidence(name):
    return link(name, E / name)

def include(path):
    """Preserve evidence text, resolve existing relative links & soften unrun passes."""
    s = path.read_text()
    def fix(m):
        label, target = m.group(1), m.group(2).strip('<>')
        if target.startswith(('http:', 'https:', '#')): return m.group(0)
        target, _, fragment = target.partition('#')
        p = Path(target)
        if not p.is_absolute(): p = (path.parent / p).resolve()
        if not p.exists():
            if p == RK / 'docs/README.md': p = RK / 'README.md'
            for prefix in ('crates/', 'packages/', 'swift/', 'desktop-app-template/'):
                if prefix in target:
                    candidate = RK / (prefix + target.split(prefix, 1)[1])
                    if candidate.exists(): p = candidate; break
        line = None
        lm = re.match(r'L(\d+)', fragment) or re.search(r':(\d+)(?:-\d+)?$', label)
        if lm: line = lm.group(1)
        return link(label, p, line)
    s = re.sub(r'\[([^\]]+)\]\(([^\n)]+)\)', fix, s)
    s = re.sub(r'(?m)^# ', '## ', s)
    if path.name == 'swift-review.md':
        s = s.replace('**Pass', '**Source review satisfactory')
        s = s.replace('protocol **pass**', 'protocol **source-reviewed**')
        s = s.replace('## Confirmed findings', '## Source findings — see independent adjudication below')
        s = s.replace('and can enter shared Core ML stage instances concurrently;', 'and invoke shared Core ML stage instances concurrently; runtime thread safety remains separately unqualified;')
        s = s.replace('`RightKitLicense` | **Source review satisfactory, native gate pending**', '`RightKitLicense` | **P2 platform-label fix; native gate pending**')
        s = s.replace('shared `FileSyncStateStore` can lose state across app/extension processes (high)', 'shared `FileSyncStateStore` can lose state across app/extension processes (conditional P2)')
        s = s.replace('real catalog tests require checked-in iOS scheme', 'real catalog tests use the checked-in iOS StoreKit scheme')
        s = s.replace('No `RightKitSwift-Package` scheme is checked in.', 'No `RightKitSwift-Package` scheme is checked in; SwiftPM can synthesize package schemes, so absence alone is not a missing-scheme defect.')
    if path.name == 'apps-core-review.md':
        s = s.replace('| P0 |', '| P1 |')
        s = s.replace('| P1 | CodeRight, HeardRight, & MailRight retain old', '| Alignment | CodeRight, HeardRight, & MailRight retain old')
        s = s.replace('Add iOS privacy manifest(s) for every shipping target, verify', 'Inspect archive privacy aggregation & add required declarations for actual API/data use; verify')
    if path.name == 'apps-other-review.md':
        s = s.replace('Swift is range pin, URL lacks `.git`, & is behind required exact', 'Swift uses a range rather than a compatible exact tag; canonical record is')
        s = s.replace('violating RightKit exact-pin policy & resolving below staged', 'allowing resolution drift; review lock policy against recorded')
        s = s.replace('nested FreeCAD/Lightcraft/SEO are independent/third-party & excluded', 'nested FreeCAD is upstream; LightCraft & SEO are separately classified below')
        s = s.replace('No source-backed duplication maps to a distinct RightKit primitive beyond copied SellRight/RightSites & Membrane snapshot checkouts above.', 'This broad review did not prove additional removable primitive copies. Native migration addendum below identifies Xtool automation as a bridge-dependent candidate.')
    return s.rstrip() + '\n'

initial = json.loads((E / 'initial-state.json').read_text())
inv = json.loads((E / 'current-inventory.json').read_text())
pub = json.loads((E / 'publication-inventory.json').read_text())
installed = json.loads((E / 'installed-first-party-apps.json').read_text())
drift = json.loads((E / 'source-drift.json').read_text()) if (E / 'source-drift.json').exists() else {}

method = f'''Fresh source review, 11 October 2026. Baseline HEAD `{initial['head']}` plus existing working-tree changes. Earlier audit conclusions were excluded. Each assertion below comes from current manifests, source, local lifecycle records or this run's evidence. No product source was edited or published.

Scope covers 25 npm packages, 57 workspace Rust crates, 10 secondary Cargo packages, root/template npm manifests, 15 RightKitSwift products, CloudKit helper & recording helpers. Publication inventory counts **84 units: 71 public, 12 private, 1 internal helper**; Swift's 15 library products belong to one release unit. App discovery covers 20 current first-party product/tool checkouts, retained Membrane snapshots, archives, workflow roots & 9 installed first-party bundles. These counts describe different populations & must not be added together.

Evidence labels: **source-confirmed** means a bounded source path supports the finding; **candidate** needs execution or compatibility proof; **recorded** means local release ledger, not remote registry confirmation; **integrated** means a source call, not just a manifest entry; **qualified** requires executed target/installed evidence. Existing tests were inspected, not rerun. “Satisfactory” in package rows is a source-review verdict, never a test pass.

The native Legion run remains **incomplete**: 17 of 79 providers completed; packets include generated/cache material & bounded omissions, Blueprint graph is unavailable, & project execution lacks a host network-sandbox receipt. `legion verify` validates report consistency, not release readiness. Its 6,678 raw scanner candidates are not accepted defects. Independent bounded lens results & source reviews supply the findings in these reports.

{link('Legion Audit SKILL.md', Path('/Users/adrdsouza/.claude/skills/legion/skills/audit/SKILL.md'),59)} states: “Project-executing checks need trusted host network-sandbox receipt; without it they are `UNPROVEN`; file-only providers still run.” Its {link('execution contract', Path('/Users/adrdsouza/.claude/skills/legion/skills/audit/references/execution-contract.md'))} requires skipping builds/tests/runtime without that receipt. That is why no new package build, installed-scale benchmark or Apple simulator/device test is claimed here. Both {link('iOS', Path('/Users/adrdsouza/.claude/skills/legion/skills/ios-development/SKILL.md'))} & {link('macOS', Path('/Users/adrdsouza/.claude/skills/legion/skills/macos-development/SKILL.md'))} skills were applied. Apple native catalog/preflight & local tool versions were inspected.

Snapshot receipts: {evidence('initial-state.json')}, {evidence('current-inventory.json')}, {evidence('source-hashes.json')}, {evidence('source-drift.json')}, {evidence('native-summary.json')}. Mechanical source census is not an authoritative symbol graph. Nested fixture/vendor/cache/generated/personal-data boundaries are recorded explicitly; no absence-of-import result proves absence of runtime use.

This was a live review interval, not an immutable checkout. Of {drift.get('baseline_files',36395):,} initially hashed files, {len(drift.get('changed',[]))} changed during review; none disappeared at comparison time. Changes span RightKit, CutRight, ScrapeRight, ViewRight, RustRight & SwiftRight. Local override counts & affected dependency declarations were reread. No unchanged-source or reproducible-build claim applies to concurrently changed files; closure uses the recorded hashes & final chosen release revision.
'''

findings = [
 ('D01','P3','candidate','settings-ui','Remove unused shell peer, optional-peer metadata & dev dependency','packages/settings-ui/package.json',33,'Focused type/test & packed-consumer install; retain emitted type dependencies.'),
 ('D02','P3','candidate','desktop template','Remove direct rightkit-service dependency; protocol crate already owns it','desktop-app-template/src-tauri/Cargo.toml',41,'Compile template on supported targets, retaining protocol dependency.'),
 ('D03','P1','source-confirmed','npm publication','Publish platform-ui 0.2.2 before app-shell 0.6.1 peer floor','packages/app-shell/package.json',40,'Coherent registry-confirmed peer set & isolated install.'),
 ('P01','P1','qualification gap','all public units','Current publication receipts do not qualify this source revision','packages/release/publication-gate.mjs',34,'Exact version/HEAD, clean unit path, host results & artifact hash per release.'),
 ('P02','P1','source-confirmed','release runtime gate','README regex gate does not verify measured budgets or installed adoption','packages/release/runtime-budgets.mjs',88,'Structured measurement/status/scale artifacts checked by publish & adoption gates.'),
 ('P03','P1','qualification gap','continuous crates','All 11 named owners lack required Runtime budget sections; no fresh scale runs','docs/RUNTIME-BUDGETS.md',7,'Declare ceilings, measure on representative data, expose status & enforce receipts.'),
 ('S01','P1','source-confirmed','RightKitSpeech','Unchecked Sendable exposes mutable Parakeet vocabulary','swift/RightKitSwift/Sources/RightKitSpeech/SpeechEngines.swift',88,'Immutable or synchronized vocabulary; concurrent async/blocking stress test.'),
 ('S02','P1','source-confirmed','RightKitSpeech','Stop during legacy dictation setup can leave resources installed after teardown','swift/RightKitSwift/Sources/RightKitSpeech/LiveDictationSession.swift',145,'Generation-owned setup/cleanup & deterministic stop-during-start test.'),
 ('S03','P2','conditional design gap','RightKitCloudSync','App-group state store has no cross-process transaction ownership','swift/RightKitSwift/Sources/RightKitCloudSync/SyncStateStore.swift',73,'Single-owner lease or locked read-modify-write/merge; two-process test.'),
 ('S04','P2','source-confirmed','RightKitWatch','Unbounded Swift event stream under a stalled consumer','swift/RightKitSwift/Sources/RightKitWatch/RightKitWatch.swift',181,'Bounded buffering with explicit loss/rescan signal & stalled-reader test.'),
 ('S05','P2','source-confirmed','RightKitLicense','macOS lease activation is labeled ios','swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift',612,'Explicit platform mapping & macOS/iOS/iPadOS payload tests.'),
 ('S06','P2','forward compatibility gap','CloudKit helper','Request protocol v is emitted but ignored at helper dispatch','swift/RightKitCloudKitHelper/Sources/rightkit-cloudkit-helper/Session.swift',71,'Reject missing/unsupported versions before mutation; v0/v1/v2 vectors.'),
 ('F01','P2','conditional hardening gap','rightkit-send','Deterministic partial path can truncate an existing partial file','crates/rightkit-send/src/receive.rs',486,'Reserve unique partial with create_new, track ownership & preserve preexisting file.'),
 ('A01','P2','source-confirmed','QA ownership','Node/browser & native QA split contradicts Rust replacement comment','crates/rightkit-qa/src/lib.rs',1,'Document platform/API ownership matrix; keep both runners until parity established.'),
 ('A02','P3','compatibility decision','QTS wrapper','Deprecated qts-ggml wrapper should gain no new API','crates/rightkit-qts-ggml/src/lib.rs',1,'Check in-tree plus published consumers before retirement; preserve compatibility.'),
 ('A03','P3','consolidation candidate','send/filesystem','Private trust-store atomic writer overlaps shared filesystem owner','crates/rightkit-send/src/lib.rs',213,'Compare owner permissions, durability, Windows replacement, links & cleanup before delegation.'),
 ('M01','P1','source-confirmed','ScrapeRight','23 RightKit Cargo overrides resolve local source','/Volumes/D/claude/scraperight/Cargo.toml',36,'Publish needed versions, remove overrides, build standalone consumer.'),
 ('M02','P1','source-confirmed','CutRight','Root & nested host override 18 owners to rk-snapshot','/Volumes/D/claude/cutright/Cargo.toml',87,'Publish needed versions, remove both override blocks, resolve registry lockfile.'),
 ('M03','P2','migration required','VoiceRight','Local upstream QTS & direct ORT have no RightKit runtime integration','/Volumes/D/claude/voiceright/src-tauri/Cargo.toml',10,'QTS API/feature parity, published exact owners, no eval layout dependency.'),
 ('M04','P2','source-confirmed','SellRight/RightSites','Swift package uses from range rather than exact version tag','/Volumes/D/claude/sellright/ios/project.yml',17,'Choose compatible published tag, pin exact URL/version & verify resolution.'),
 ('M05','P2','source-confirmed','app release configs','Missing/noncanonical distribution provider values','/Volumes/D/claude/postright/right-release.config.mjs',21,'Set github-releases or r2 as appropriate; preserve per-product storage config separately.'),
 ('U01','P2','source-confirmed','media-ui','Image stage keyboard handlers sit on an unfocusable div','packages/media-ui/src/image/ImageViewerStage.tsx',102,'Focusable accessible stage with visible focus & keyboard zoom/pan/fit checks.'),
]
rows=[]
for i,priority,status,owner,title,path,line,acceptance in findings:
    p=Path(path);p=p if p.is_absolute() else RK/p
    rows.append(dict(id=i,priority=priority,status=status,owner=owner,finding=title,path=str(p),line=line,acceptance=acceptance))
(O/'findings.json').write_text(json.dumps(rows,indent=2)+'\n')
with (O/'handoff-backlog.csv').open('w') as f:
    w=csv.DictWriter(f,fieldnames=rows[0].keys());w.writeheader();w.writerows(rows)
def finding_table(ids=None):
    picked=[r for r in rows if ids is None or r['id'] in ids]
    return '| ID | Priority / status | Owner & finding | Acceptance |\n|---|---|---|---|\n'+'\n'.join(f"| {r['id']} | {r['priority']} · {r['status']} | {link(r['owner'],r['path'],r['line'])}: {r['finding']} | {r['acceptance']} |" for r in picked)+'\n'

r1='''# 1. Dependencies & publication readiness

**Decision:** perform two bounded dependency removals after focused validation. Keep `windows-core`: current proc-macro expansion uses it. Publication & runtime evidence gaps are more significant than dependency pruning.

'''+method+'\n## Prioritized decisions\n\n'+finding_table({'D01','D02','D03','P01','P02','P03'})+'''
## Dependency review boundaries

Npm review covers 27 manifests: root, 25 packages & desktop template. Rust review covers 57 workspace crates plus 10 secondary packages, with 445 direct dependency edges. Target conditions, optional features, renamed crates, workspace inheritance, proc macros, build scripts, tests & emitted TypeScript declarations are separate use classes. A type-only or macro-generated import can still require a dependency.

All direct edges remain available in '''+evidence('rightkit-dependencies.csv')+', '+link('Rust edge ledger',O/'rust-dependency-edges.json')+''' & per-package rows below. Swift package has no external third-party package dependency; its target edges, helper edge & app pins are in report 2/3. No transitive vulnerability, license-compliance, outdated-registry or binary-size clearance is claimed.

Older app pins are alignment inventory, not automatic defects. Unpublished newer source must not be substituted into consumers merely to make versions match. `registry-recorded` values require registry/archive verification in publication work.

'''+include(E/'npm-review.md')+'\n'+include(O/'rust-review.md')+'\n'+include(E/'dependency-skeptic.md')+'\n'+include(E/'publication-review.md')+'\n'+include(E/'runtime-budget-review.md')
r1+='''
## Publication order & durable closure

1. Finish source/contract decisions & fix confirmed correctness defects in report 2.
2. Establish runtime budgets for all continuous owners, including omitted process watchdog/model leases; enforce structured evidence at both publication & adoption.
3. Validate each changed package on applicable target/feature matrix, then release contracts. Required repository commands include focused builds/tests, `pnpm --dir rightkit test:release`, `pnpm --dir rightkit doctor:release` & app parity contract tests from suite root.
4. Produce standalone package archives/install receipts. Publish foundational dependencies before consumers; platform-ui/app-shell requires explicit order. Cargo publication stays behind RightKit release command.
5. Record immutable release versions, archive hashes & verified availability. Each app then applies report 3's bounded handoff & acceptance checks.

Close each backlog ID with source revision, execution command, environment, artifact path/hash, result & consuming app version. Reopen only affected IDs when their inputs change.
'''
(O/'01-dependencies-and-publication.md').write_text(r1)

r2='''# 2. Package boundaries, duplication & Apple review

**Decision:** keep current public package facades. Source review does not justify a broad package split. Reconcile QA ownership, contain deprecated QTS compatibility, compare send/fs persistence contracts, & fix concrete lifecycle/state/buffering issues inside existing owners.

'''+method+'\n## Prioritized decisions\n\n'+finding_table({'A01','A02','A03','S01','S02','S03','S04','S05','S06','F01','U01'})+'\n'+include(E/'architecture-review.md')+'''
## Internal decomposition priorities

Public package split & internal component decomposition are different decisions. Keep published entry points stable while making resource/state ownership explicit:

| Component | Internal boundary to tighten | Why / acceptance |
|---|---|---|
| Swift Speech | Generation-owned capture session & serialized/immutable decode configuration | S01/S02: single cleanup owner, stop/start interleavings & concurrent decode tests. |
| CloudSync persistence | Cross-process transaction or elected writer, separate from transport actor | S03: an actor serializes only its process; ownership must cover load/modify/save. |
| Watch/index/service/jobs | Bounded ingress/subscriber queues, admission limits & visible pressure counters | Declare overflow/rescan/reject semantics; benchmark stalled consumers & connection storms. |
| Release readiness | Evidence schema/validator separate from README presentation | P02: bind real measurements, workload, platform & source identity to qualification. |
| Send persistence | Protocol caller separate from filesystem durability policy | A03/F01: shared atomic writer only after permission/rename/durability parity; unique owned partials. |
| UI media stage | Focus/keyboard interaction contract explicit in stage component | U01: keyboard reachability becomes independently testable without app-specific wrappers. |

No automatic merge is justified for `update`/`updates`, JS/native logging or licensing, shell/app-shell, speech client/inference, jobs/scenario, or filesystem/index/search. Language, process & policy boundaries are meaningful. Similar names alone are not duplication.

'''+include(E/'swift-review.md')+'\n'+include(E/'swift-skeptic.md')+'''
## Parent adjudication of additional lenses

* **S05 retained:** `activateLease` sends `platform()` output & macOS takes non-UIKit `ios` branch. Impact is incorrect host metadata; server-side rejection was not reproduced.
* **S06 retained at P2:** helper ignores `v`, while current Rust client emits version 1. This is forward-compatibility validation debt, not evidence that current v1 client corrupts CloudKit data.
* **F01 retained conditionally:** pre-existing predictable partial file is truncated after receiver accepts matching transfer. It requires collision/stale-file state; no remote arbitrary-path write was demonstrated. Use exclusive unique partial ownership.
* **U01 retained:** image stage has no `tabIndex`; its parent only registers global text-deletion keys. Zoom/pan/fit handlers need focusable stage. Audio artwork's button role also lacks its own keyboard activation, but surrounding MediaPlayer already has keyboard playback shortcuts; classify that as a lower-priority semantic cleanup, not inaccessible playback overall.
* **Naming proposal rejected:** `@rightkit/update` is repository maintenance & `@rightkit/updates` is application update policy. No concrete misuse warrants rename or merge.
* **Performance proposals remain benchmark scenarios:** daemon whole-state writes, per-event flush, cache full-key eviction sort, sync full-local enumeration & send discovery workers have source cost/bound implications. Fresh load measurements are needed before calling them observed regressions.

'''+include(E/'send-skeptic.md')+'\n'+include(E/'platform-parity-review.md')
r2+='\n## Additional pressure scenarios\n\n'+evidence('lenses/reasoning.resilience.json')+' & '+evidence('lenses/reasoning.performance-notes.md')+''' record unbounded Rust watch pending paths, fsindex ingress, Unix pre-hello service workers & jobs event subscribers. Review queue/admission ownership & include slow-consumer/reconnect-storm workloads in P03. These are bounded source observations; no measured exhaustion incident is attributed to this run.

Maintain one contract owner per capability, preserve host-specific adapters, & close each source defect with focused regression evidence before publishing its next version.
'''
(O/'02-boundaries-duplication-and-apple.md').write_text(r2)

app_rows=[
('CodeRight','coderight','Integrated; engine/desktop/context/iOS differ','Secrets/keyring seam, coherent OCR/ORT/FS/watch, iOS target/privacy verification'),
('HeardRight','heardright','Integrated; native helpers remain local','Service/lease/log/model seams; macOS helper extraction; Apple family acceptance'),
('ViewRight','viewright','Broad integration','Watcher/worker adapter boundary, published cohort & large-library runtime proof'),
('MailRight','mailright','Integrated older cohort','DB/SQLCipher, cache, OAuth migration with mailbox-scale proof'),
('ScrapeRight','scraperight','Integrated through 23 local overrides','M01 first; network/parser & model/group seams; iOS/share/helper verification'),
('CutRight','cutright','Integrated through 18 overridden owners','M02 first; cache/jobs/service budget & verified media artifacts'),
('GenRight','genright','Integrated; several ledger-staged pins','Published QTS/ORT/models/jobs/logs set; long-session installed evidence'),
('VoiceRight','voiceright','Release tooling only','M03: upstream local QTS/direct ORT to compatible published owners'),
('WorkRight','workright','No RightKit integration found','Candidate DB bootstrap & durable logs; preserve capture/retention/consent policy'),
('Pulse','pulse','Core search/DB, hub fsindex/UI integrated','Installed full-volume fsindex qualification; platform-specific fallback'),
('SwiftRight','swiftright','Own SDK/bridge; no RightKitSwift integration','Qualify ABI/packaging; adopt only required Apple products with API parity'),
('Ember / Lightcraft branch','ember','Shell/UI/QA integrated','Logger seam, compatible published shell/control & installed idle/watchdog metrics'),
('Legion','legion','Release bridge integrated; other tooling declared','Compatible workflow/release tooling updates; retain native audit engine'),
('RustRight','rustright','Ownership/fs/process integrated','Coherent registry contracts & lock policy; process runtime proof if watchdog enabled'),
('SellRight','sellright','Legal + Swift diagnostics','M04 exact Swift pin; canonical release provider; preserve commerce policy'),
('RightSites','rightsites','Downstream SellRight code','M04 plus separate product identity/provider; coordinate shared changes'),
('Xtool','xtool','No RightKit integration found','Native helper/Node compatibility mapping; no forced Rust adoption'),
('PostRight','postright','Broad template integration','Product placeholders, canonical provider, published cohort & installed runtime receipts'),
('LightCraft workflow checkout','workflows/lightcraft','No RightKit integration found','Separate earlier product checkout from Ember; choose maintained source before migration'),
('SEO tool','workflows/seo','No RightKit integration found','Independent Python/CLI/MCP owner; no proven compatible RightKit migration obligation'),
]
(O/'app-handoffs.json').write_text(json.dumps([dict(app=a,path=str(Path('/Volumes/D/claude')/p),status=s,next_work=n) for a,p,s,n in app_rows],indent=2)+'\n')
r3='''# 3. Machine app inventory & migration handoffs

**Decision:** remove consumer source overrides first, then migrate proven shared primitives using compatible published versions. Keep app policy local. App-wide “adopted” status requires source integration, portable resolution & applicable installed runtime proof.

'''+method+'''
## Scope & census

Fresh mechanical census found 225 Git roots under inspected development locations. Detailed manifest/reference census spans 32 scoped roots after excluding `.git` metadata as a false root: 619 manifests, 4,882 dependency declarations across apps/RightKit, 6,835 lexical RightKit references & 36,395 source/config hashes. Counts include nested fixtures, forks & duplicate retained snapshots; they are not counts of independent apps or proven integrations.

Primary discovery: `/Volumes/D/claude`, `/Volumes/D/worktrees`, Codex worktrees & relevant home development locations. Shallow expansion covered `/Volumes/D/archive`, `sr0-build`, `rightkit-publish`, `tmp`, plus home `claude`, Hyperframes, Sites, CodeBuddy, WorkBuddy AI, Desktop & Documents. Caches, vendor/third-party implementations, session data & personal content are excluded. '''+evidence('machine-repositories.json')+' & '+evidence('machine-scope-addendum.json')+''' record identities & exclusions. RightKit publication clones, old HeardRight release clone & SwiftRight staging clones are not separate products. Three Membrane snapshots count as one retained consumer family.

This is a first-party development/adoption inventory. Installed third-party software without owned source is not a RightKit migration target.

## Handoff index

| App / source | Current state | Next bounded work |
|---|---|---|
'''
r3+='\n'.join(f'| {link(a,Path("/Volumes/D/claude")/p)} | {s} | {n} |' for a,p,s,n in app_rows)+'\n'
r3+='''
Manifest/version cells below are evidence snapshots, not instructions to upgrade every old dependency. Prefer compatible recorded releases; verify availability before changing pins. A missing package declaration does not establish a need for that capability. Privacy-manifest observations require checking final archive aggregation & actual API use before adding declarations.

'''+include(E/'apps-core-review.md')+'\n'+include(E/'apps-growth-review.md')+'\n'+include(E/'apps-other-review.md')
if (E/'native-migration-review.md').exists(): r3+='\n'+include(E/'native-migration-review.md')
r3+='\n## Installed bundle inventory\n\nBundle metadata only; installed versions may differ from working source.\n\n| Bundle | Version | Build | Identifier |\n|---|---|---|---|\n'
r3+='\n'.join(f'| {link(Path(x["path"]).name,x["path"])} | {x["version"]} | {x["build"]} | `{x["id"]}` |' for x in installed)+'\n'
r3+='''
Passive process samples are recorded separately. They do not exercise full-volume crawl, active microphone sessions, mailbox scale, slow clients or per-crate isolation, & therefore cannot qualify a runtime budget. Match installed build, workload & public status output before collecting the acceptance receipt.

Samples: '''+evidence('runtime-sample-1.json')+', '+evidence('runtime-sample-2.json')+', '+evidence('runtime-sample-3.json')+'''. Pulse process identity changed between samples 1 & 2; do not infer an optimization trend across that restart. Samples 2/3 show native Pulse PID 34020 at 179.4/51.1 MiB RSS & 41.2/1.4% ps CPU, while hub PID 34240 shows 51.4/42.7 MiB & 0.0/0.1%. This is unscripted whole-process observation, with no per-crate attribution or controlled workload.

## Handoff order

1. **RightKit chat:** D03/P01–P03, Swift source defects & release cohort closure.
2. **ScrapeRight & CutRight chats:** publish required owners, delete every shared-source override, prove standalone resolution.
3. **Pulse, HeardRight, GenRight, ViewRight, MailRight & PostRight chats:** expose runtime status & record relevant installed scale workload after owner qualification.
4. **VoiceRight chat:** replace local QTS/direct ORT through measured API parity.
5. **Apple consumer chats:** exact tagged package linkage, helper extraction decisions, extension/watch/device acceptance & archive privacy/entitlement inspection.
6. **Other product chats:** apply only source-backed compatible migrations in their rows; retain product schemas, UX, consent & business policy.

These handoffs are prepared, not sent. Copy each app section with its related backlog IDs & require an evidence receipt for closure.
'''
(O/'03-app-inventory-and-migration.md').write_text(r3)

readme='''# Fresh RightKit audit — 11 October 2026

**Source audit redone from current code. Publication is not yet qualified.** Both iOS & macOS skills were used; previous audit conclusions were excluded.

| Report | Contents |
|---|---|
| '''+link('1. Dependencies & publication',O/'01-dependencies-and-publication.md')+''' | Every package/crate inventory, direct-edge review, removals, release lifecycle & runtime-budget proof |
| '''+link('2. Boundaries, duplication & Apple',O/'02-boundaries-duplication-and-apple.md')+''' | All 25 npm/57 Rust/15 Swift product boundaries, decomposition decisions & source defects |
| '''+link('3. App inventory & migration',O/'03-app-inventory-and-migration.md')+''' | First-party checkouts, nested targets/helpers, installed apps, actual use & per-chat handoffs |

Two dependency-removal candidates survived independent review: unused settings-ui shell peer/dev edge & template's redundant direct service edge. `windows-core` stays because Windows macro expansion needs it.

Release blockers: 23 ScrapeRight & 18 CutRight local Cargo overrides; source/staged/recorded versions differ; no current public-unit publication receipts; all 11 named runtime owners lack required budget sections. Source findings include Swift Sendable/lifecycle/shared-state/buffering defects, macOS lease labeling & helper protocol validation.

Keep public package boundaries. Reconcile QA ownership documentation, contain deprecated QTS wrapper, tighten resource/state ownership & compare send/filesystem durability before consolidation.

''' + method + '\n## Prioritized handoff backlog\n\n' + finding_table() + '\nMachine-readable: '+link('findings.json',O/'findings.json')+', '+link('handoff-backlog.csv',O/'handoff-backlog.csv')+', '+link('app-handoffs.json',O/'app-handoffs.json')+'''.

Start with release evidence & runtime gates, then move ScrapeRight/CutRight onto published owners using prepared handoffs.
'''
(O/'README.md').write_text(readme)
print(json.dumps({'reports': [str(O/n) for n in ['README.md','01-dependencies-and-publication.md','02-boundaries-duplication-and-apple.md','03-app-inventory-and-migration.md']], 'backlog':len(rows),'apps':len(app_rows)}))
