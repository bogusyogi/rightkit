# Locked RightKit implementation — seven handoff packets

Status: **jointly settled & frozen for implementation handoff**, 2026-10-20. Astra high independently rechecked all seven corrected packets: **ready**. All 39 Fable high findings are adjudicated in [review register](review.md); final assurance is in [Astra high report](review-astra-high.md). Tracking: [rightkit#822](https://github.com/bogusyogi/rightkit/issues/822). These documents plan implementation; they do not certify code, publish packages or migrate apps.

Adrian's objective, supplied verbatim to independent review:

> my goal is all apps on shared packages and minimal to no separate code, unless absolutely necessary with proper evidence as to why that specific app needs that code and not the others.

Normative input: [signed M1–M23 adjudication](../2026-10-20-rightkit-spec-adjudication.md), commit `5ce6e2c32843edd28b003162f31f50f6cce0d9b5`, SHA-256 `f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49`. Its amendments override both originals. Required semantics remain in [Astra K01–K16](../2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md); row structure is archived in [Fable r1](../2026-10-20-rightkit-spec-lock/fable-r1.md). Names of unimplemented APIs are provisional; behavior, ownership, refusal/recovery requirements & exceptions are locked.

## Dispatch

Give each future executor its linked packet, repository access & signed spec references. Package executor implements missing owners, qualifies them, publishes scoped releases & produces verified handoff evidence. App executors consume only published releases satisfying their own task prerequisites.

| Packet | Checkout / responsibility | Main mechanics retired from apps |
|---|---|---|
| [01 Packages](01-packages.md) | `D:\Claude\rightkit` — PK01–PK17 implementation, qualification, publication & immutable manifest | Supplies qualified replacements required by six packets |
| [02 Pulse](02-pulse.md) | `D:\Claude\pulse` | Root cutover, storage/host orchestration, service lifecycle, approval mechanics, native/Swift shared behavior |
| [03 HeardRight](03-heardright.md) | `D:\Claude\heardright\tauri-app-next` plus declared native targets | DB/key rotation/recovery, credential wrappers, licence/session flow, duplicate stall/window/helper orchestration, test-selector fallback |
| [04 ViewRight](04-viewright.md) | `D:\Claude\viewright` | Save lock/atomic-write helpers, token/header bridge, generic PDFKit/preview/window/host/logging mechanics |
| [05 MailRight](05-mailright.md) | `D:\Claude\mailright` | DB restore/relocation choreography, pending/approval/audit mechanics, raw licence bridge & generic host glue |
| [06 ScrapeRight](06-scraperight.md) | `D:\Claude\scraperight` | Supervisor snapshot sequencing, cookie-artifact workflow, settings/save orchestration, window sizing/reveal, onboarding/model glue |
| [07 CodeRight](07-coderight.md) | `D:\Claude\coderight` | Daemon lifecycle implementation, process identity/attachment mechanics, token/lease/header bridge & generic host/logging glue |

Deletion is function-level after every caller reaches qualified replacement. Preserve schema, product data, declarative configuration & actual domain actions. ViewRight's content-only inspector is explicit permanent exception. Third-party compatibility patches require exact evidence & sunset. Platform specificity or one current consumer alone does not justify local shared mechanics.

## Execution order

1. Package executor inventories actual published bytes versus candidate source. Keep shipped build/test fixes & contained layout; remove unnecessary unpublished additions. Published mistakes require corrected newer releases, never rewritten versions or downgrade.
2. Implement each capability's missing work, resolving actual dependency edges before selecting final versions. Complete safety-critical foundations first where dependents need them. Package receipt must distinguish candidate exports from verified published exports.
3. Qualify final package bytes on required hosts/features, publish in actual dependency order, then fetch & verify registry bytes plus standalone consumer. A satisfying existing published dependency needs no gratuitous republish.
4. Release task readiness per qualified capability slice & AP task. PK01–PK17 statuses summarize progress; they never gate adoption. Each app task names `requiresCapabilities` with exact features/targets; only its consumed subset must qualify. Separately deferred #821 never blocks this delivery. Authenticated enforcement activation follows qualified owners without gating those same receipts.
5. App executor pins exact qualified releases, migrates production callers, deletes old mechanics, verifies behavior/refusal/recovery & records justified remnants. Adoption may finish incrementally; whole-app completion requires every applicable row.

[Package packet §2](01-packages.md#2-dependency-dag-and-publishing-order) owns dependency edges & publication order. PK04/06/08/10/11/13 may modify one shell package: combine accepted edits into coherent package release; never race publishers or release incomplete public composition. PK17 enforcement activation follows qualified owners & existing authenticated trust-sync rules.

## Handoff & acceptance

`handoff/published-release-manifest.json` is a **future execution output**, intentionally absent during planning. Its schema is defined once in package packet §6. Immutable `publish` or `qualify-existing` snapshots bind signed spec, verified source identity where available, exact versions, fetched archive integrity, feature/target qualification, capability receipts & AP prerequisites. Existing satisfying releases qualify without republishing or inventing historical publisher evidence. Snapshot hash travels with app packet; later snapshots do not silently alter an already pinned receipt.

An API name, version declaration, compilation or lexical export scan proves neither adoption nor runtime behavior. Every completed app task records published owner/API/features, production caller route, exact deleted symbols, decisive behavior evidence & retained functions with reasons. An unmet capability blocks its dependent task only. Conditional requirements need source-backed applicability decisions; an N/A task is excluded from adoption readiness, never reported as migrated.

Use repository-declared scripts & managed native toolchains. One integration owner per checkout runs checks; children edit disjoint files only. No app distribution/upload is authorized by these packets. Package publication happens only when user dispatches package execution with publication scope & existing signing/publication requirements are satisfied.

## Evidence baseline

Heads re-read on 2026-10-20; unrelated working-tree changes exist. Commit IDs do not identify dirty candidate bytes. Future executor refreshes primary checkout safely, runs required setup/doctor & rechecks cited symbols before editing.

| Repository | Read HEAD |
|---|---|
| RightKit | `5ce6e2c32843edd28b003162f31f50f6cce0d9b5` |
| Pulse | `08ada52deafc8f895658fbd7ce93825e5a142ee9` |
| HeardRight | `fa82ecc295189565c4bdc162dc106009d7e036b3` |
| ViewRight | `b7e91c03642e0ddf89657adefe5a5eb2386f5aec` |
| MailRight | `4d835646e79f64ddfd52afd1a7d4c378b19dc04b` |
| ScrapeRight | `8ac9b1aac62075bc54fd530774af56949193ce10` |
| CodeRight | `15b3ac93e7896a94be913a21c592154e898589df` |

## Adversarial review

Astra challenges Fable's package plan; Fable challenges app packets & this index. [Review register](review.md) records disagreements, evidence, corrections & final disposition. Planning completion requires resolved outcome findings, full M1–M23 coverage, consistent PK/AP dependencies & unchanged signed-spec hash.

Final coverage: 17 package work groups, 66 independently qualified capability slices & 98 app tasks (Pulse19, HeardRight21, ViewRight14, MailRight12, ScrapeRight17, CodeRight15). Every app packet covers all23 M rows. Existing exception ledgers are reconciled per entry; missing shared behavior stays required owner work. Unwired CodeRight canvas preserves intended domain schema/actions without accidental feature activation; generic mechanics receive no permanent exception.

Adrian authorized Fable to commit/push frozen packets into their respective repositories & file each as an issue, then execute package qualification/publication. Fable owns delivery; issue bodies bind packet hashes, repository commits & immutable RK references. App execution starts only for its qualified capability receipts.

## Frozen packet hashes

These seven hashes identify reviewed implementation instructions; index/review metadata is delivered separately.

| Packet | SHA-256 |
|---|---|
| [01-packages.md](01-packages.md) | `495db109dd7299c90e524e9ca96ef9401adcae9a429bc6e579a0338b8152d982` |
| [02-pulse.md](02-pulse.md) | `29a0af8f7c2fc7a8d53f40ff9cf92bf3c774df050a394d3892613c0db5715a6f` |
| [03-heardright.md](03-heardright.md) | `a357c61d0b08fd3828fd888336d04db21b52a707e4bb39c9d411393a32e9a191` |
| [04-viewright.md](04-viewright.md) | `44bda6433c341fd298506946300b0b077cd7969d36f6805ee8ee8eb712e1c9ac` |
| [05-mailright.md](05-mailright.md) | `b1acd68c7abba836b9cc7e7c9e3946052adb3e8537daa0622ac10b25d59d96c5` |
| [06-scraperight.md](06-scraperight.md) | `4ba6ba0965a7907a6d31790562b63e8ef98f5b3d46e203beaa6726e5c2e4dc1e` |
| [07-coderight.md](07-coderight.md) | `34f32e8c14275738a142f0b792d515ca948250afbf6ce692164f3d1bb17ab3fe` |
