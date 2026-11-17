# What goes in RightKit and what stays in the app

**Locked decision, 2026-11-16.** This file is the authority on where code lives. Where any
other document, mapping table, changelog or handoff says something different, this file
wins. Read it before moving any app code into RightKit and before changing any UI package.

## The decision

1. **All apps' code lives in RightKit.** That includes code only one app uses.
2. **The mandatory packages (rows M1 to M23) are shared by every app.** A new app starts
   from them as its base: the shell, the engine, the sidecar, onboarding, EULA and legal,
   settings, licence, logging, crash reporting, updates, release, QA and shared
   components. The list is `packages/release/ownership-mandatory-set.json`.
3. **App-specific capability is a RightKit component.** HeardRight's pill is a component.
   Pulse's notch is a component. A media player used by ScrapeRight, CutRight and GenRight
   is a shared component. A component may be used by one app, two, three, all, or none.
4. **How an app looks does not move into RightKit.** "Exactly how HeardRight looks" does
   not come out of HeardRight and go into RightKit. Each app keeps its own thin
   customization on top of the RightKit packages.
5. **Any component can be customized visually by the app that uses it.** This is a
   property every RightKit component must have.
6. **The app's share of code is kept small, and that is about code.** CSS, theming, copy
   and composition are the app's and do not count against it.

## The rule, by kind

A percentage cannot tell a stylesheet from a state machine. Decide by kind.

**An app repository may contain:**

- Theme and styling: tokens, CSS, fonts, brand assets.
- Copy: its strings and wording.
- Composition: which pages exist and their order, which onboarding steps and settings
  sections it uses, how it arranges the mandatory shell.
- Configuration it passes to RightKit, declared exceptions, and its journey tests.

**An app repository may not contain logic:** state machines, data handling, calls to the
engine, business rules. That is code, and code goes to RightKit.

**A RightKit package may not contain an app:** one app's screen set, its composition of
the shell, onboarding or settings, its styling, its class names, its wording or its
brand data.

## The contract every RightKit component meets

- No product name anywhere in it.
- No hard-coded colours or fonts. It reads the host's theme variables.
- All text comes in as labels, with neutral defaults.
- The app can restyle it and replace its parts.
- No app-specific class names.

The mandatory packages (`react-shell`, `settings-ui`, `onboarding`, `license`,
`app-update`, `platform-ui`) are the reference for this contract.

## When an app needs something a mandatory package lacks

Add the generic piece to the mandatory package. Do not put it in the app's own package,
and do not wrap the mandatory package in an app-shaped one.

## How progress is measured

- Not by lines deleted from an app.
- An app is on RightKit when it composes the mandatory packages directly, takes its
  capability from RightKit components, and holds only what the list above allows.
- The app's share of code can be reported as a gauge, counted on logic only (CSS, theme,
  copy and composition files excluded). It is never a gate and never a target to hit by
  moving look.

## What this supersedes

- "Shell. At most 2% per app" (`docs/audits/2026-11-01-rightkit-package-master-plan/DECISIONS.md`,
  item 8), the same figure in `DECISIONS-ADDENDUM.md` and `PORT-EXECUTION.md`, and the
  0.5% and per-app percentages in the master plan tables. The figure was about code. It
  was applied to look.
- Every row of the per-app mapping tables (`docs/audits/2026-11-01-rightkit-package-master-plan/per-app/`
  and each app's `docs/rightkit-inventory/`) that sends an app's pages, shell, onboarding
  flow, settings layout, styling or wording into a RightKit package.
- Any changelog or handoff line that justifies a move with "so an app can delete its own
  copy".

## State on 2026-11-16

These packages hold an app's look or composition and do not yet meet this file:
`dictation-ui` (HeardRight), `video-ui` (CutRight), `mail-ui` and `workspace-ui`
(MailRight), the `agent-ui-*` packages (CodeRight), `social-ui` (PostRight),
`document-workspace` (ViewRight), `library-ui` queue screens (ScrapeRight) and
`system-ui` (Pulse). CutRight and MailRight consume theirs on `main`.

`node scripts/preflight.mjs --only shared-behavior-presentation,layer-reach` checks
the extracted video workspace, project list, progress, phrase search, Shorts actions,
framing review, mail sidebar and compose settings behavior. It flags product names,
app-prefixed classes and common colour literals in the extracted files. The layer
policy checks that local runtime imports from the new entry points stay clear of
legacy page markup, CSS and app theme stores. Extend coverage as more wrappers are
split. The `desktop-app-template` first-run scenario checks that the rendered legal
gate and shell respond to two host palettes. Run it on an installed template artifact
to qualify that coverage. The general app logic check remains open.

## Active migration scope

The default mandatory check covers HeardRight, ViewRight, Pulse, CodeRight, GenRight,
MailRight, ScrapeRight, CutRight and PostRight. Include applicable targets of those
apps. Ember is deferred. SellRight is N/A. Legion remains independent and may use
specific shared tools where they fit. The port matrix reports dependency currency
against published versions. Use production call sites, removed code and installed
qualification to establish adoption.

M22 applies to all nine desktop apps. Each exposes its capabilities through a headless
CLI in the release artifact, callable by `right-ctl` while the GUI is closed. Pulse calls
apps through that route. [RightKit #994](https://github.com/bogusyogi/rightkit/issues/994)
records the decision. [RightKit #1658](https://github.com/bogusyogi/rightkit/issues/1658)
tracks the implementation. If an app lacks this route, record the gap and keep M22 applicable.

The [preserved CLI design](https://github.com/bogusyogi/mailright/blob/e97c9e1533904df99d91f445c33ca8fada228c4b/docs/contracts/design/SHAPE.md)
defines the contract, host-first routing and installed proof journeys. Apply the
active app scope above. Installed-path and signature checks must bind the executable
to trusted product signing identity before it runs. The user-writable discovery
record is a locator. Keep each app's existing licence, pairing and approval gates.

## How the earlier reading went wrong

The full analysis, with the timeline, the root causes, every app checked against the
mandatory packages and the audit data behind it, is in
`docs/audits/2026-11-16-app-boundary/ANALYSIS.md`.

## Where this came from

The owner, in session `session_01QWpjiWBTssha215BSFHDHg` on 2026-11-16, after finding that
five apps' screens had been moved into RightKit packages:

> "i want all apps code in rightkit, yes. there are 21 or 23 mandatory packages m1-m23
> these are shared among all apps ... Then, if there is code specific to a particular
> app, like HerdWrite has a pill, the pill will be a HerdWrite kit component. ... However,
> that doesn't mean that exactly how HerdWrite looks will come out of HerdWrite and go
> into Right Kit. ... Each app will maintain its own thin customization, its own things
> on top of the Right Kit packages. This means the theming, the colors, and how
> HerdWrite looks belong in HerdWrite, not in Right Kit."

> "the colors of anything can be customized ... any component, across anything, can be
> customized visually."

> "yes i asked for the % to be low, but that didn't mean css and theming. it meant code."

It agrees with decision 1 of 2026-11-01 ("product-specific means only how each app looks
& composes the shared shell") and with the 2026-10-20 HeardRight plan, task AP-HR-08
("retain dictation-specific pages, model choices and transcript workflow").
