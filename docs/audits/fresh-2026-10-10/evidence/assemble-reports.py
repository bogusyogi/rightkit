"""Assemble three requested Markdown reports from fresh source-review evidence."""
from pathlib import Path
import json, re

E=Path(__file__).resolve().parent
O=E.parent
R=E.parents[3]

def link(name):return f'[{name}]({E/name})'
def section(name, title):
    t=(E/name).read_text()
    t=re.sub(r'^# .+\n', '', t, count=1)
    t=re.sub(r'^(#{2,5}) ',r'#\1 ',t,flags=re.M)
    return f'\n## {title}\n'+t+'\n'

stamp='''Audit date: 10 October 2026, Asia/Kolkata. RightKit baseline: `08252a681d9f9dc2911962a49fa0026a4c345dff`, with existing uncommitted OCR/fsindex & lockfile changes. Findings use current source; no previous audit findings were reused. Source edits, publication & app migration are outside this report delivery.

Evidence labels: **observed** = manifest/source fact; **candidate** = proposed removal or consolidation requiring focused verification; **keep** = justified boundary; **unproven** = unavailable runtime/registry evidence. Source version, staged version, recorded registry version & adopted baseline are distinct lifecycle states.
'''

coverage='''## Coverage & verification status

| Dimension | Result |
|---|---|
| RightKit source inventory | All 24 npm package manifests & 56 workspace crate manifests enumerated; 26 npm manifests including workspace root/template, 66 Rust package manifests including 10 fixtures/examples/template packages. |
| Declared dependencies | 171 npm declarations & 437 Rust direct edges, retaining runtime/dev/build/peer/optional/target/feature scope. Swift & .NET helpers listed separately. |
| App census | 22 current/retained checkout roots, 260 npm/Cargo RightKit declarations, 3,109 lexical references across 20,718 hashed files; semantic review distinguishes imports, adapters, comments & generated assets. |
| Duplication scan | 1,042 first-party source/e2e files, 229,545 significant lines; exact 16-line windows identify candidates, followed by source-body review. No AST-complete clone claim. |
| Apple skill follow-up | Both Legion iOS & macOS skills applied; all 14 Swift products, helpers, template/spike & Apple consumers reviewed. macOS suite: 201 cases/3 skips/0 failures; iOS product build passed; StoreKit simulator: 8 pass; CloudKit helper built; recorder helper: 14 cases/1 skip/0 failures; contract parity: 2 pass. Old archive helper: 1 pass/2 failures. Exact logs & closure checks are linked below. |
| Other runtime/package qualification | No new Windows qualification, actual packed-registry-artifact installation or live registry verification accepted. Native Apple checks supplement source review, not app/device/distribution qualification. |
| Native Audit gate | **INCOMPLETE; quality gate UNPROVEN.** 78 providers returned results, only 16 marked complete; no reasoning receipt ingested. Graph unavailable; source byte limits hit; lens packets failed with `plan digest differs from the executed plan`. `legion verify` failed: `native application config is required`. |

Frozen Audit runner output is recorded separately, not treated as verified product findings. Direct source review & separately executed Apple checks supply inventories, recommendations & bounded native evidence. Use Apple closure checklist for remaining repairs, then qualify selected published versions & app integrations.
'''

apple_links=f'''## Apple follow-up completed

Both requested Apple skills were applied after initial report delivery. All 14 products & native consumer/helper boundaries now have explicit verdicts. Native checks passed except obsolete archive-helper tests; source review identified privacy, cancellation, persistence & concurrency fixes. Product source remains unchanged.

- {link('apple-verification.md')} — commands, results, compiler warnings, skill/toolchain provenance & qualification scope.
- {link('apple-closure.md')} — one prioritized backlog with owners & exact acceptance checks; use this for follow-up instead of restarting full audit.

Concurrent workspace delta: Pulse now declares `@rightkit/release 0.2.146` (original snapshot: `0.2.141`) & adds source/installed QA receipt mappings. Its installed plan still records failure pending a release-safe driver. Concurrent edits affect Pulse, SwiftRight, Legion & RightKit release/Git tooling; original broad audit remains a source snapshot. Audited Swift package/helper source hashes remain unchanged. Original machine-readable inventories retain snapshot values; {link('workspace-drift.json')} records newer Pulse dependency value, commit & changed-file hashes for handoff. No other captured package dependency declaration changed at this follow-up snapshot.

'''

dep='''# 1. Dependencies & publication prerequisites

'''+stamp+'''
## Decisions

- **No external runtime npm dependency is confirmed unnecessary in 24 shared packages.** Imports, optional entrypoints & public `.d.ts` requirements justify declarations; every direct edge appears in inventory below.
- **One workspace Rust removal candidate:** `rightkit-daemon` directly declares `rustls-pki-types`, while code uses `rustls::pki_types`. Remove only after focused TLS/build validation. Three additional Rust candidates live in examples/template, not shipped shared libraries.
- **Template cleanup:** `@rightkit/tauri` is declared before license/device wiring exists; JS `@tauri-apps/plugin-process` has no frontend use found. Native process plugin is separate & should remain where used.
- **Retain generated dependencies:** `windows-core` is required by `#[implement]`; `ggml_sys` is renamed from `rightkit-qts-ggml-sys`; public type dependencies & peer/dev pairs are justified. Avoid token-only deletion.
- **Reduce dependency weight through features first:** capture, inference/vector, PDF fonts/editing, TTS client, shell engine & consented HTTP paths have distinct needs. Default-feature changes require consumer feature-matrix proof; no measured binary-size saving is claimed.
- **New publication from current HEAD fails local receipt requirements.** This does not mean already recorded versions are absent from registries. Reconcile intended source/staged versions, qualify selected release closure, then publish & verify registry artifacts before changing apps.

## Exact machine-readable inventory

'''+f'''- {link('npm-dependencies.csv')} — all 171 declarations with exact versions/specs, scope & manifest loci.
- {link('rust-dependencies.json')} — all 437 edges, source-reference evidence, optional/default-feature flags & target scopes.
- {link('rightkit-manifests.json')} — complete 24 npm + 56 Rust manifests.
- {link('swift-publication.csv')} — all 80 npm/Cargo units & three Swift packages, including source/staged/adopted/local-registry/receipt comparison.

'''
dep+=section('npm-dependencies.md','npm: every package, dependency use & portability')
dep+=section('rust-dependencies.md','Rust: every crate, dependency use, features & duplication')
dep+=section('swift-publication.md','Swift products, helpers & publication evidence')
dep+='''
## Other native manifests

`ownership-approval/windows/OwnershipApproval.csproj` has no NuGet PackageReference; it targets .NET 10 Windows Forms & self-contained `win-x64` output. `spikes/rk2-windows/rk2.csproj` declares four direct packages: WebView2 `1.0.4258.31`, CsWinRT `2.3.1`, Vortice.Direct3D11 `3.8.3`, Vortice.DXGI `3.8.3`. All have source use: `TargetApp.cs:5`, `Native.cs:106`, `Capturer.cs:13,119,148`, `Shot.cs:8,63`. Keep all four in this experimental spike. No Python requirements/pyproject or Go module manifests were found in audited first-party RightKit tree.

## Publication sequence

1. Set intended package/version closure; preserve 11 Cargo `publish=false` units & two private npm packages until distribution intent explicitly changes.
2. Resolve source changes & source/staged discrepancies; regenerate derived inventory from canonical manifests. Older adopted baselines are not themselves defects.
3. Build/test selected package features on required native hosts through approved RightKit paths; inspect actual packed npm/crate/Swift artifacts. Verify exports, optional peers, Cargo normalized dependencies, vendored native inputs & external helper resources.
4. Generate receipts for exact canonical commit & artifact bytes. Preserve no-network feature variants, Windows compatibility aliases, SQLite/SQLCipher modes & Apple helper contracts.
5. Publish through authorized release workflow; verify remote versions & artifact hashes, update local records, then send app-specific version targets from Report 3.

'''+coverage
dep=dep.replace('## Decisions\n',apple_links+'## Decisions\n',1)
dep+=section('apple-verification.md','Apple skill review & executed verification')
(O/'01-dependencies.md').write_text(dep)

arch='''# 2. Package boundaries, duplication & decomposition

'''+stamp+'''
## Decisions

Keep existing runtime/domain boundaries. Current evidence supports internal consolidation & clearer subpaths before creating more published packages.

| Priority | Action | Reason |
|---|---|---|
| First | Consolidate durable atomic-write semantics around `rightkit-fs` after compatibility fixtures. | Multiple independent writers differ in Windows replacement, parent sync, private permissions & retries. Models' revisioned writer deletes Windows destination before rename; this is observable source-level loss of old-target preservation if rename then fails. |
| First | Unify PID-liveness policy after Windows/Unix edge-case tests. | FS & service disagree on PID bounds & Windows access denial; stale-state cleanup needs one conservative policy. |
| Next | Reuse shared model verification/digest primitives for TTS where API parity exists. | Avoid another QTS artifact identity/hash/cache authority; retain TTS worker/runtime facade. |
| Next | Consolidate copied browser E2E harness into QA. | Three byte-identical copies plus settings-specific fork already share Chrome lifecycle owner. |
| Next | Expose narrow HTTP & FFmpeg execution seams. | Keep domain-specific retry, redirect, streaming, consent, cancellation & cache policy. |
| Next | Move Tauri suite maps into compatibility subpath; isolate large implementation responsibilities internally. | Preserve public version lines while clarifying generic API ownership. |
| Keep | Generated ownership-policy copies, runtime adapters, Swift/Rust verification, model layouts & QA language layers. | Existing synchronization, platform contracts or independent state ownership justify these boundaries. |

Ownership-source remains generated & synchronized; retain existing Adrian-authenticated trust-sync activation contract.

'''
arch+=section('architecture.md','Complete npm/Rust inventory & boundary verdicts')
arch+=section('duplicate-review.md','Body-level duplication adjudication')
swift=(E/'swift-publication.md').read_text()
start=swift.find('## Swift inventory');end=swift.find('## Consumer inventory')
if start>=0:
    t=swift[start:end if end>=0 else None]
    t=re.sub(r'^(#{2,5}) ',r'#\1 ',t,flags=re.M)
    arch+='\n## Swift products & cross-runtime boundaries\n\n'+t
arch+='''
## Decomposition acceptance

Each proposed consolidation must preserve errors, persisted formats, privacy/permissions, retry/cancellation, feature opt-outs & platform behavior. Keep public facades during migration. Delete old implementations only after consumer-level parity fixtures pass; package splitting needs demonstrated dependency or release-cadence benefit, not line count.

'''+f'Duplicate candidate evidence: {link("duplicate-candidates.json")}; rerun scanner: {link("scan-duplicates.py")}.\n\n'+coverage
arch=arch.replace('## Decisions\n',apple_links+'## Decisions\n',1)
arch+=section('apple-core-review.md','Apple core: eight product verdicts & source findings')
arch+=section('apple-ui-review.md','Apple UI/platform: six product verdicts & source findings')
arch+=section('apple-closure.md','Apple remediation acceptance')
(O/'02-package-boundaries.md').write_text(arch)

apps='''# 3. App adoption inventory & handoffs

'''+stamp+'''
## Handoff rules

“Declared” is not “fully adopted.” Tables separate source use, local policy wrappers, duplicated primitives, version drift & unimplemented targets. A missing dependency does not automatically require migration. Use only verified published coherent versions; staged values below describe intended release work.

Source census covers 17 named primary repositories, two workflow checkouts & three retained Membrane worktrees. FreeCAD is third-party fork/reference; PostRight is docs-only; Membrane copies represent one product. ScreenRight archive & missing Citadel are explicitly recorded. Build copies, research clones, caches & installed third-party applications are not separate RightKit consumer projects.

## Snapshot manifest census

| Checkout | Distinct declared RightKit npm/Cargo names | Manifests declaring them | Lexical references | Source identity |
|---|---:|---:|---:|---|
'''
d=json.loads((E/'app-manifest-inventory.json').read_text())
for root in d['roots']:
    name=root['app'];es=[x for x in d['manifest_edges'] if x['app']==name];rs=[x for x in d['references'] if x['app']==name]
    apps+=f'| `{name}` | {len(set(x["dependency"] for x in es))} | {len(set(x["manifest"] for x in es))} | {len(rs)} | `{root["head"][:12] or "retained worktree; Git metadata unavailable"}` |\n'
apps+='''
Counts include workspace/dev/build declarations; lexical references can include comments or types. Swift products are enumerated in per-app sections. Zero declared npm/Cargo packages is not proof of zero shared functionality.

## Routing & priorities

| App/group | Handoff focus |
|---|---|
| CodeRight | Desktop/context dependency closures; old fs/watch/http/db versions; generated iOS vs project.yml pin consistency; shared mechanisms behind local policy. |
| HeardRight | UI generation alignment; speech/service/process/model versions; Apple products & Overlay usage; retain audio/ASR product policy. |
| ViewRight | Native logs/db/http/secrets/OCR; evaluate PDF/playback mechanisms separately from document fidelity. |
| MailRight | SQLCipher-safe db/embed/OAuth/http/secret updates; preserve Gmail schema, key namespace & content security. |
| ScrapeRight | Broad UI/native alignment; shared supervisor/processing candidates; Swift helpers; preserve extraction/model compatibility. |
| CutRight & GenRight | Preserve media/engine policy; align existing shared APIs; evaluate remaining transcript/process/model mechanisms. |
| VoiceRight & WorkRight | Largest initial-adoption surface: native inference/process/model & storage/secret/capture primitives; qualify API compatibility first. |
| Pulse & SwiftRight | Mixed native/UI targets; avoid unsupported toolchain/runtime swaps; preserve bridge ABI & service protocol. |
| Legion & RustRight | Release/AX source ownership & metadata; exact consumer version specs; keep broker/orchestrator policy local. |
| SellRight & RightSites | Exact Swift declarations plus legal/diagnostics use; no forced desktop UI packages. |
| Xtool & LightCraft | Runtime-compatible adoption only; larger native bindings remain explicit design work. |
| Membrane, ScreenRight, Citadel, PostRight | Restore/select current implementation source before assigning migration; distinguish retained, archived, absent & docs-only states. |

'''
apps+=section('apps-core.md','Five core apps: pins, implementations & handoff checks')
apps+=section('apps-new.md','Newer products: pins, implementations & handoff checks')
apps+=section('apps-other.md','Tooling, web/mobile products, retained & absent checkouts')
apps+='''
## Every RightKit package/crate: direct-consumer matrix

This matrix is manifest-level only. “None found” can mean transitive consumption, private/internal intent, helper invocation, future product work or no applicable app; it is not dead-code evidence. Three Membrane copies collapse to one product here.

| Package/crate | Source version | Direct declaration consumers |
|---|---|---|
'''
m=json.loads((E/'rightkit-manifests.json').read_text())
for kind in ['npm','cargo']:
    for p in m[kind]:
        consumers=sorted(set('membrane (retained)' if x['app'].startswith('membrane:') else x['app'] for x in d['manifest_edges'] if x['dependency']==p['name']))
        apps+=f'| `{p["name"]}` | `{p["version"]}` | {", ".join(consumers) or "None found in scoped npm/Cargo manifests"} |\n'
apps+='\n## Exact handoff evidence\n\n'
for name in ['app-rightkit-dependencies.csv','app-rightkit-references.csv','app-manifest-inventory.json','machine-repositories.json','collect-inventory.py']:
    apps+='- '+link(name)+'\n'
apps+='''
Each app chat should receive its full section, exact declaration rows, selected verified package versions & acceptance checks. No messages have been sent to other chats. Start alignment only after selected RightKit package closure has release evidence & standalone-install proof.

'''+coverage
apps=apps.replace('## Handoff rules\n',apple_links+'## Handoff rules\n',1)
apps+=section('apple-consumers-review.md','Apple targets, helpers, exact pins & native integration handoffs')
(O/'03-app-adoption.md').write_text(apps)

index='''# Fresh RightKit audit — 10 October 2026

Three source-led reports, prepared independently of prior audits:

'''
for name,label in [('01-dependencies.md','Dependencies & publication prerequisites'),('02-package-boundaries.md','Package boundaries, duplication & decomposition'),('03-app-adoption.md','App adoption inventory & handoffs')]:
    index+=f'- [{label}]({O/name})\n'
index+='''
Coverage: 24 npm packages, 56 workspace crates, Swift products/helpers, secondary native/template manifests & 22 local checkout roots, plus archive/absent-product entries. Exact versions, dependency scopes, source evidence & per-app acceptance checks accompany reports.

Priority: resolve release/version evidence, validate narrow dependency removals, consolidate duplicated mechanisms with parity fixtures, then distribute app-specific adoption work.

Apple follow-up now applies both requested iOS & macOS skills to all 14 Swift products, helpers & consumers. macOS suite passed (201 cases, 3 skips); all-product iOS build passed; StoreKit simulator passed 8 tests; CloudKit helper built; recording helper passed 13 tests with 1 expected skip; license contract parity passed 2 tests. Legacy archive helper failed 2 tests. Swift source findings now have one prioritized closure checklist.

Frozen native Audit certification remains **INCOMPLETE / UNPROVEN**. Apple build/test evidence is established separately; live registry, signed app/device & cross-platform publication qualification remain distinct work.

Fresh evidence, rerunnable inventory scripts & source hashes are under `evidence/`. Existing product changes were preserved.
'''
index+=f'\n[Initial verification results]({E/"verification.json"}) — original inventory counts, citation checks & source-hash stability.\n'
index+=f'\n[Apple native evidence]({E/"apple-verification.md"}) · [Apple closure checklist]({E/"apple-closure.md"}) · [Updated report verification]({E/"apple-verification.json"})\n'
index+=f'\n[Concurrent workspace delta]({E/"workspace-drift.json"}) — Pulse release pin advanced to `0.2.146`; original inventory remains baseline. RightKit product source was preserved.\n'
(O/'README.md').write_text(index)
for p in [O/'README.md',O/'01-dependencies.md',O/'02-package-boundaries.md',O/'03-app-adoption.md']:
    print(p.name,p.stat().st_size)
