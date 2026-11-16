# RightKit: what was asked, what was built, and where it went wrong

Written 2026-11-16. Evidence is from the repositories as checked out in this session:
RightKit `main` at `4b23c987`, and each app's `main` and its adoption branch
(`claude/hopeful-hypatia-x956lw`). Nothing in this document was built or run; every
statement is from reading manifests, source, changelogs and git history.

---

## 1. The owner's intent: the locked decision

**Locked on 2026-11-16.** Recorded in RightKit as `docs/APP-BOUNDARY.md`, in
`decisions.md`, and as a locked invariant in the agent rules. Where any plan, mapping
table, changelog or handoff says something different, this wins.

**The rule, by kind of code (not a percentage):**

- **An app repository may contain** its theme and styling (tokens, CSS, fonts, brand
  assets), its copy, its composition (which pages exist and their order, which onboarding
  steps and settings sections it uses, how it arranges the mandatory shell), the
  configuration it passes to RightKit, declared exceptions, and its journey tests.
- **An app repository may not contain logic:** state machines, data handling, calls to
  the engine, business rules. That is code, and code goes to RightKit.
- **A RightKit package may not contain an app:** one app's screen set, its composition of
  the shell, onboarding or settings, its styling, its class names, its wording or its
  brand data.
- **Every RightKit component:** no product name, no hard-coded colours or fonts (it reads
  the host's theme variables), all text passed in as labels with neutral defaults,
  restylable and with replaceable parts, no app-specific class names.
- **When an app needs something a mandatory package lacks,** the generic piece is added to
  the mandatory package. It does not go into an app-shaped package.
- **Progress is measured** by whether an app composes the mandatory packages directly and
  holds only what the first bullet allows. Never by lines deleted, never by a percentage.

**The intent behind it, in the owner's terms.** Everything below is measured against it.

1. **All apps' code lives in RightKit.** Yes, including code only one app uses.
2. **The mandatory packages (M1 to M23) are shared by every app.** A new app starts from
   them as its base: the shell, the engine, the sidecar, onboarding, EULA, settings, and
   shared components.
3. **App-specific capability is a RightKit component.** HeardRight's pill is a component.
   Pulse's notch is a component. A media player used by ScrapeRight, CutRight and GenRight
   is a shared component. Something may be used by one app, two, three, all, or none.
4. **How an app looks does not move into RightKit.** "Exactly how HeardRight looks" is
   not supposed to come out of HeardRight and go into RightKit. Each app keeps its own
   thin customization on top of the RightKit packages: theming, colours, its pages and how
   they are arranged, its composition of the mandatory shell, onboarding and settings.
5. **Any component can be customized visually by the app that uses it.** That is a
   property every RightKit component must have, not a special case.
6. **The app's share of code should be small. That is about code, not about CSS and
   theming.** (Clarified by the owner on 2026-11-16.) Logic and components go to
   RightKit; the app's styling, theme and composition stay with the app and do not count
   against it.

---

## 2. What went wrong, in one paragraph

Sessions treated "the app's look" as "capability" and moved it into RightKit. For five
of the nine apps, the app's own screens, shell composition, onboarding flow and styling
were copied into a RightKit package "so an app can delete its own copy", and the app was
rewired to import its own look back from RightKit. Progress was measured by lines deleted
from the app. Two apps have this on `main` already (CutRight, MailRight). Three have it
on an unmerged branch (HeardRight, CodeRight, PostRight). The mandatory packages
themselves were adopted long ago and mostly still are; the damage is that apps now reach
them through an app-shaped wrapper instead of composing them directly, and the mandatory
packages were never extended with the generic pieces apps actually needed.

---

## 3. Timeline, with evidence

| Date | What happened | Evidence |
|---|---|---|
| 2026-10-20 | Signed plan for HeardRight: replace generic mechanics, **keep** dictation pages and UI. Task AP-HR-08: "Replace generic shared settings/theme/platform components; retain dictation-specific pages, model choices and transcript workflow." | `docs/plans/2026-10-20-rightkit-implementation/03-heardright.md` |
| 2026-11-01 | Master plan and "port commission" written. The plan text says "The app keeps only its look and composition of the shared shell". The same documents cap what stays in an app at **2%**, and their per-app tables put the figure at 0.5% overall (HeardRight 1.5%, about 7,000 lines). | `docs/audits/2026-11-01-rightkit-package-master-plan.md:27`, `.../DECISIONS.md:13`, `.../PORT-EXECUTION.md:3` |
| 2026-11-01 | The per-app mapping tables assign each app's pages and onboarding screens to a RightKit UI package. | `.../per-app/heardright-rightkit-mapping.md:31-34,46` and the equivalents for the other apps |
| 2026-11-03 | **CutRight `main`** takes `@rightkit/video-ui` "for the editor and project UI; delete the app copies". **MailRight `main`** ports its "web side" to `@rightkit/mail-ui`. | CutRight `8c0ec25`, MailRight `40a7d15` |
| 2026-11-03 | An addendum records the commission as "each app keeps only a thin shell (<=2%)". It was written by a session to preserve chat decisions, and says its own reconstruction "may be incomplete". | `.../DECISIONS-ADDENDUM.md:3,8` |
| 2026-11-11 | A shape review judges `@rightkit/dictation-ui` "the wrong shape: one app's whole front end". Version 0.2.0 removes the app frame, licence, theme, legal and settings code, and a list of "HeardRight app code with no place in a dictation package". It keeps the pages. It is marked "pending the owner's confirmation". | `packages/dictation-ui/CHANGELOG.md` (0.2.0) |
| 2026-11-13 to 14 | HeardRight's branch moves pages, history, settings sections, onboarding and shell pieces onto `dictation-ui`. In RightKit, versions 0.2.5 to 0.2.7 add HeardRight's licence and account sections, its first-run setup, and its shell footer and feature reel, each "so an app can delete its own copy". 0.2.7 re-adds items 0.2.0 had removed three days earlier. | 20 HeardRight commits, 14 RightKit commits, all from session `01Tn3haBy59tpjyvAYBqJB2g` |
| 2026-11-16 | This session inherits the goal "make RightKit the single home of every app's shared and single-app code ... with its duplicate code deleted", extends the same approach on the Rust side and for GenRight and CodeRight, and does not review the npm packages until asked what `dictation-ui` is. | this session |

---

## 4. Root causes

1. **A rule about code was applied to look.** The owner asked for the app's share of
   code to be low; the documents record it as "Shell. At most 2% per app"
   (`DECISIONS.md:13`). The same decision record says "product-specific means only how
   each app looks & composes the shared shell" (`DECISIONS.md:1`). Sessions counted
   everything in the app's frontend against the 2%, including CSS, theme and page
   composition. HeardRight's frontend is about 22,300 lines; 2% of HeardRight in the
   plan's own table is about 7,000. Counted that way the cap cannot be met while the app
   keeps its look, so the look was moved too: markup, class names, wording and styling
   were copied into RightKit "equal to HeardRight's shipped" version.
2. **The mapping tables decided it file by file.** Once a table said "HeardRight pages →
   dictation-ui", every later session treated the move as settled.
3. **Progress was measured by deletion.** Goals were phrased as "duplicate code deleted",
   changelog entries justify additions with "so an app can delete its own copy", and
   reports counted lines removed. Nothing measured whether an app was composing the
   mandatory packages directly.
4. **The mandatory packages had real gaps, and nobody filled them.** When an app needed a
   generic piece the mandatory package lacked (a tabbed settings page, a licence dialog,
   account and device sections), the piece went into the app's wrapper package instead of
   into the mandatory package. See section 6.
5. **Checkpoints were passed without the owner.** The 2026-11-11 redraw was "pending the
   owner's confirmation" and proceeded. The standing rule in handoffs was "never ask
   questions; decide, act, report after".
6. **Handoffs carried the goal sentence, not the intent.** This session received one
   sentence and a task list, and did not check either against the signed plan or the
   shape review that were sitting in the repository.

---

## 5. The nine apps against the mandatory packages

### 5.1 Are the mandatory packages ported? (each app's `main`)

Method: for every owner package of rows M1 to M20 and M22 in
`packages/release/ownership-mandatory-set.json`, I checked whether the app declares it
(any `package.json` or `Cargo.toml`) and counted source lines that import it.
"Declared, never imported" means my search found no import line in the app's source; I
did not open each case, so it can also mean the package is used through CSS, config or
another package.

| App | Owners not declared | Declared, never imported |
|---|---|---|
| HeardRight | agent-tools (M22, ratified not applicable) | `@rightkit/tauri`, `@rightkit/qa`, `@rightkit/qa-chrome` |
| ViewRight | job-queue (M3, M4), sidecar (M4), app-settings (M10), http (M17), agent-tools (M22) | none |
| Pulse | none | `@rightkit/qa-chrome` |
| CodeRight | agent-tools (M22) | `@rightkit/tauri` |
| GenRight | none | `@rightkit/tauri`, `@rightkit/release`, `@rightkit/qa-chrome` |
| MailRight | agent-tools (M22) | `@rightkit/theme`, `@rightkit/legal`, `@rightkit/release` |
| ScrapeRight | agent-tools (M22) | `@rightkit/tauri`, `@rightkit/legal-ui` |
| **CutRight** | db (M9), http (M17) | sidecar, `@rightkit/tauri`, **`platform-ui`, `settings-ui`, `license`, `legal`, `onboarding`, `app-update`**, `qa`, `qa-chrome` |
| PostRight | agent-tools (M22) | sidecar, `@rightkit/tauri`, `@rightkit/theme`, `@rightkit/qa`, `@rightkit/qa-chrome` |

Reading it:

- **The mandatory port did happen.** Eight of nine apps declare and import almost every
  owner on `main`.
- **CutRight is the exception.** Its own frontend imports none of the mandatory settings,
  licence, legal, onboarding or update packages. Its whole UI is inside
  `@rightkit/video-ui`, which uses them internally. CutRight does not compose the
  mandatory base; a RightKit package does it on CutRight's behalf.
- **ViewRight** lacks the engine rows (M3, M4), Rust settings (M10) and HTTP (M17).
  Whether those are genuinely not applicable to a viewer was not checked.
- Only HeardRight, CodeRight and GenRight list their adopted rows in
  `rightkit-mandatory.json`. Pulse declares only M22. ViewRight, CutRight, MailRight and
  ScrapeRight declare only not-applicable rows. PostRight mentions the rows in a comment.
- The mandatory list itself is stale: versions date from 2026-10-19, M21 and M23 to M25
  have no owner assigned, and M6 still names `rightkit-daemon` although the code moved to
  `rightkit-roots`.

### 5.2 Where the regression is

"Own UI" is the app's TypeScript, TSX and CSS under `src`. The last column counts import
lines of the app-shaped RightKit packages.

| App | Own UI on `main` | Own UI on branch | App-shaped RightKit package | Where |
|---|---|---|---|---|
| **CutRight** | 681 | 685 | `video-ui` (10,000 lines of CutRight UI) | **on `main` since 2026-11-03** |
| **MailRight** | 4,723 | 4,725 | `mail-ui` (19,100 lines), 116 import lines | **on `main` since 2026-11-03** |
| **HeardRight** | 22,265 | 4,872 | `dictation-ui`, 0 → 50 import lines | branch only |
| **CodeRight** | 99,307 | 75,438 | `agent-ui-*`, 0 → 329 import lines | branch only |
| **PostRight** | 18,517 | 1,259 | `social-ui`, 0 → 9 import lines | branch only |
| ViewRight | 134,805 | 134,679 | none imported | not regressed |
| Pulse | 16,952 | 16,952 | none imported | not regressed |
| GenRight | 28,245 | 28,245 | none imported | not regressed |
| ScrapeRight | 15,824 | 15,824 | none imported | not regressed |

For ViewRight, Pulse and ScrapeRight, RightKit already contains copies of their screens
(`document-workspace`, `system-ui`, `library-ui/queue`) that the apps do not use yet. The
copies were made; the rewiring was not.

Direct use of the mandatory frontend packages fell on the branches that regressed
(import lines, `main` → branch): HeardRight `react-shell` 16 → 13, `settings-ui` 8 → 4,
`onboarding` 4 → 2, `app-update` 5 → 3; CodeRight `react-shell` 89 → 71, `license` 3 → 0;
PostRight `react-shell` 27 → 5.

On the Rust side, direct imports of the mandatory crates also fell on several branches
(for example GenRight `rightkit-db` 25 lines → not declared). That is a different thing:
the app's capability code moved into a RightKit capability crate, and that crate uses the
mandatory crate. It matches the intent.

---

## 6. RightKit itself

From a classification of all 52 npm packages and 228 crates. About 190 crates were
classified from their manifest and who depends on them, not from reading source.

| Class | npm packages | Lines | Crates | Lines |
|---|---|---|---|---|
| Mandatory | 16 | 134,000 | 17 | 235,000 |
| Shared capability | 4 | 10,700 | 66 | 380,000 |
| App capability, correctly in RightKit | 5 | 24,600 | 68 | 431,000 |
| An app's look, should not be there | 2 | 10,000 | 0 | 0 |
| Mixed: capability plus an app's look | 12 | 204,000 | 0 | 0 |
| Tooling | 7 | 19,200 | 6 | 24,800 |
| Used by no app | 6 | 66,800 | 71 | 693,000 |

**What is sound.** The Rust crates. No crate was found that mixes capability with an
app's look. `rightkit-notch-ui` is an example of an app component done as intended.

**What is wrong.** The npm UI packages:

- `dictation-ui` (16,100 lines): 7,500 capability (the pill is 2,760), 4,500 HeardRight
  look, 4,100 generic pieces that belong in mandatory packages.
- `video-ui/app`: CutRight's whole UI, 10,000 lines.
- `mail-ui`: MailRight's UI, 19,100 lines including 4,800 of CSS.
- The seven `agent-ui-*` packages: about 103,000 lines of CodeRight's UI.
- `document-workspace/src/app` (ViewRight shell, 17,200), `library-ui/queue` (ScrapeRight
  screens, 7,400), `social-ui/screens` (PostRight, 7,000), `system-ui/views` (Pulse,
  4,300), `workspace-ui/views` (MailRight, 2,400).

**Components that cannot be customized.** The pill hard-codes 26 colours and lets the app
override one class. That breaks point 5 of the intent. It was not checked for the other
components.

**Gaps in the mandatory base.** These generic pieces exist in no mandatory package, so
HeardRight kept or copied its own, and they ended up in `dictation-ui`:

- a tabbed settings page, and the link from settings search to the command palette
- a licence lifecycle dialog (`@rightkit/license` has only the state logic)
- account sign-in and device-management sections
- a sidebar update-status line
- a hover tooltip
- a permissions step for onboarding
- the onboarding step counter counts a hidden step, so HeardRight hides it with CSS

**Other defects.**

- 1,157 lines of code or data in mandatory and shared packages name a specific app (for
  example per-app clients exported from `@rightkit/tauri`, an app list in
  `rightkit-models`).
- `library-ui` bundles a list, a Markdown reader and ScrapeRight's screens in one entry.
  HeardRight's History uses the list and receives all of it, which is how a maths parser
  with a security advisory reached HeardRight's build.
- Eight documents state the "at most 2% stays in the app" model.

---

## 7. HeardRight in detail

The adoption branch has 54 commits.

| Verdict | Commits | Lines changed | Content |
|---|---|---|---|
| Keep | 38 | 20,300 | Mandatory rows adopted directly; Rust capability moved into RightKit |
| Redo | 10 | 12,300 | Pages, history, settings sections, onboarding, shell moved into `dictation-ui` |
| Mixed | 6 | 3,000 | A correct change bundled with look moved into the wrapper |

- **Rows used directly on both `main` and the branch:** M1 (Rust), M3, M4, M5, M7, M8,
  M9, M11, M13, M15, M16, M17, M19, M20.
- **Real adoptions the branch added:** data roots (M6), licence lifecycle and account
  session (M12), updates (M18), shell hardening (M1).
- **Rows that went from direct to wrapper on the branch:** onboarding (M14; on `main`
  HeardRight used `@rightkit/onboarding` directly with 15 step files of its own),
  settings (M10), parts of the frame (M2), licence screens (M12), update status (M18).
- **Nothing was lost.** The roughly 13,800 lines of HeardRight's look that were moved are
  still on HeardRight's `main`.
- **The Rust commits are separable.** The 24 Rust keep-commits share no file with the
  redo commits apart from two ledger files.
- **RightKit's own app template shows the intended pattern works.** HeardRight's shell,
  onboarding, settings panel and updates on `main` can be expressed as "mandatory package
  plus the app's own thin config" today.

---

## 8. What this session did

- Merged ten RightKit pull requests and published about 40 crate versions. All of it is
  Rust. The classification in section 6 found the Rust side consistent with the intent,
  so none of it is known to need reverting. That is a finding of the audit, not a
  verified result.
- Added four Rust swaps to HeardRight's branch (text pipeline, command host, two ASR
  crates). They are in the "keep" group. None has been built on macOS or Windows.
- Added commits to CodeRight's and GenRight's branches on the Rust side.
- Did not review the npm packages, did not check the inherited goal against the signed
  plan or the 2026-11-11 review, and reported progress as lines deleted.
- Sent a worker to redo GenRight work that was already done, and ran with idle workers
  for about two hours.
- Left open and unmerged: RightKit pull request 1654.

---

## 9. What is not known

- Nothing here was compiled or run. "Uses the owner directly" means the source imports
  it, not that the app builds or behaves the same.
- The "declared, never imported" entries in 5.1 were not opened one by one.
- Whether ViewRight's and CutRight's missing rows are genuinely not applicable.
- The per-row state of CodeRight, PostRight, MailRight and CutRight was not audited at
  the depth HeardRight was. Section 5 gives counts for them, not a commit-by-commit
  verdict.
- Swift, Android and iOS code in every app.
- Whether the 77 packages and crates that no app uses are wanted.

---

## 10. Repair, in order

1. **Fix the record first.** Replace the 2% rule and the per-app mapping tables with the
   intent in section 1, in RightKit's documents, so no later session repeats this.
2. **Fill the mandatory gaps** listed in section 6, in the mandatory packages.
3. **Make every component customizable by its app**, starting with the pill.
4. **Split the app-shaped packages.** Capability stays as components; each app's look
   goes back to the app; generic pieces go to the mandatory packages. `dictation-ui`
   first, then the others.
5. **HeardRight:** new branch from `main`; carry the 24 Rust commits and the 7 frontend
   commits that adopt mandatory packages directly; finish the mandatory rows on their
   owners; then take the pill and dictation hooks from the slimmed package.
6. **CutRight and MailRight:** these are on `main`. Their look has to come back out of
   `video-ui` and `mail-ui` into the apps.
7. **CodeRight and PostRight:** the same verdict pass HeardRight got, before any more
   work on their branches.
8. **Bring the mandatory list up to date** and have every app declare its rows.

Full audit data: `rightkit-audit.md`,
`heardright-audit.md`,
`app-mandatory-matrix.json` and `app-branch-commits.json` (all in this folder).

---

## 11. Gaps closed on 2026-11-16 (checked directly, after the first version of this document)

### 11.1 "Declared, never imported": each case opened

| Package | Apps | What it actually is |
|---|---|---|
| `@rightkit/tauri` (M1) | HeardRight, CodeRight, GenRight, ScrapeRight, PostRight | **Not adopted in practice.** Declared in `package.json`, no import. CodeRight and ScrapeRight record why in `.rightkit/ownership-exceptions.json`: the package "has no typed command registry" and "no swappable invoke/listen/webview/window layer", so the apps keep their own invoke layer. A gap in the mandatory package. HeardRight's branch does adopt it. |
| `@rightkit/theme` (M2) | MailRight | **Not adopted in practice.** MailRight's theme catalogue is its own; the exception file says "MOVE @rightkit/theme". |
| `@rightkit/theme` (M2) | PostRight | Adopted: `src/brand.css` is generated by the package from the app's brand file. |
| `@rightkit/qa`, `@rightkit/qa-chrome` (M20) | HeardRight | **Declared only.** No script or config uses them; HeardRight's own M25 row says there is no `journeys.toml` yet. |
| `@rightkit/qa-chrome` (M20) | Pulse | Used by a QA script package. |
| `@rightkit/release` (M19) | GenRight, MailRight | Used through git hooks and release config, not imports. Adopted. |
| `@rightkit/legal` (M14) | MailRight | Declared only; MailRight has its own `LegalGate` component and imports only `legal-ui`'s CSS. |
| `@rightkit/legal-ui` (M14) | ScrapeRight | No direct import and no legal code in `src`; the gate comes through `@rightkit/onboarding`, which runs it. Adopted indirectly. |
| `rightkit-sidecar` (M4) | PostRight | Declared in `Cargo.toml`, never used. |
| CutRight's settings, licence, legal, onboarding and update packages | CutRight | CutRight's own frontend is 7 files; it imports `video-ui`, `react-shell`, `theme`, `logs`, `media-ui`. Everything else is composed inside `video-ui`. |

### 11.2 Rows not declared

- **ViewRight** records M3, M4, M7, M17 and M22 as not applicable with reasons (no engine
  process, no local service, no HTTP of its own, no agent surface) and ratification
  requests that expire 2026-11-30. M10 on the Rust side is not declared and not excused:
  its settings store is `rightkit_document_workspace::settings::SettingsStore`, which
  uses `rightkit-app-settings` internally. So the mandatory crate is used, through a
  capability crate.
- **CutRight** records M9 (no database; project state is JSON through `rightkit-fs`) and
  M17 (no HTTP client of its own) as not applicable, ratification requested, expiring
  2026-11-30.

### 11.3 Every app's branch, commit by commit

Scripted over all commits on each adoption branch that are not on `main`: frontend lines
added and deleted, Rust lines, and whether the commit starts importing an app-shaped
RightKit UI package.

| App | Branch commits | Commits moving the app's UI onto an app-shaped package | Frontend lines deleted by them | Rust lines deleted on the branch |
|---|---|---|---|---|
| HeardRight | 54 | 10 redo, 6 mixed (section 7) | about 13,800 | 16,000 |
| CodeRight | 23 | 6, dated 2026-11-14 and 15 (`e950d18`, `ec00d26`, `f22974d`, `aa95691`, `8818b2e`, `1374723`) | 24,300 | 58,600 |
| PostRight | 15 | 1, dated 2026-11-14 (`5568a19`) | 17,300 | 11,100 |
| GenRight | 23 | 0 | 0 | 53,100 |
| Pulse | 32 | 0 | 0 | 91,500 |
| ViewRight | 24 | 0 | 0 | 700 |
| ScrapeRight | 23 | 0 | 0 | 13,500 |
| MailRight | 8 | 0 on the branch | 0 | 7,900 |
| CutRight | 10 | 0 on the branch | 0 | 8,300 |

On `main`, the two merged moves:

| App | Commit | Date | Frontend files | Frontend lines deleted |
|---|---|---|---|---|
| CutRight | `8c0ec25` | 2026-11-03 | 78 | 16,900 (views, timeline, inspector, workspace) |
| MailRight | `40a7d15` | 2026-11-03 | 223 | 24,500 (views, store, IPC, 14 CSS files) |

So the frontend moves happened in two waves: CutRight and MailRight on 3 November
(merged), then HeardRight, CodeRight and PostRight on 13 to 15 November (unmerged).
GenRight, Pulse, ViewRight and ScrapeRight had Rust work only.

### 11.4 Can the app restyle what it gets from RightKit?

Hard-coded colour literals against theme variables, per package source.

| Package | Hex colours in TS | Hex colours in CSS | `var(--…)` uses in CSS |
|---|---|---|---|
| `react-shell`, `settings-ui`, `onboarding`, `license`, `app-update`, `platform-ui` (mandatory) | 0 | 0 | none shipped; styling comes from the host's tokens |
| `mail-ui` | 141 | 52 | 582 |
| `library-ui` | 12 | 79 | 397 |
| `document-workspace` | 17 | 44 | 298 |
| `agent-ui-core` | 14 | 44 | 101 |
| `video-ui` | 7 | 15 | 426 |
| `system-ui` | 10 | 3 | 189 |
| `dictation-ui` | 1 | 1 | 17 |

`dictation-ui` scores low here because its look is in class names, not colour literals:
it carries 372 `hr-` class tokens and Tailwind class strings whose rules live in
HeardRight's stylesheet, plus the pill's 390-line style table. The mandatory packages are
the ones already built the intended way.

### 11.5 Native code

| App | Swift lines | Kotlin/Java lines | Uses RightKit Swift packages |
|---|---|---|---|
| Pulse | 107,400 | 1,700 | yes |
| HeardRight | 48,000 | 900 | yes |
| CodeRight | 40,600 | 0 | yes |
| ScrapeRight | 9,200 | 0 | yes |
| CutRight | 2,000 | 0 | none found |
| ViewRight, GenRight, MailRight, PostRight | 0 | 0 | not applicable |

Which Swift code is generic, which is capability and which is the app's look was not
classified. That needs the same pass the TypeScript got.

### 11.6 Still not known

- Nothing was compiled or run.
- The Swift and Kotlin classification above.
- Whether the 77 packages and crates that no app uses are wanted.
- About 190 crates were classified from manifests and dependents, not source.

