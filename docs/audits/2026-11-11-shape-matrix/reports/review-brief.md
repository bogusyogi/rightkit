# Brief: best-shape review of one RightKit package family

Read-only. Do not modify, build, commit or push anything in any repository. Write only the two output files named in your task.

## Why

RightKit (`/home/user/rightkit`) is the shared library for a suite of apps. Each app is meant to move the majority of its code into RightKit crates (Rust), npm packages (`@rightkit/*`) and Swift modules (RightKitSwift), keeping only a thin shell. The owner's goal, in his words: "find the best shape for rightkit, for all apps to move majority code to rk, but each crate/ package must be in the best shape. Create the full matrix then publish crates and packages". Earlier he said: "What I had asked the agent to do was to check the cross app code and decide the best shape that all apps should adopt. HeardRight is the live app, followed by viewright and pulse is a product I use often, everything else is tbd and some like cut and post were started just before I decided on the right kit move", and "Final shape is no duplicate code".

The existing plan (`/home/user/rightkit/docs/audits/2026-11-01-rightkit-package-master-plan.md`, with `per-app/<app>-rightkit-mapping.md` beside it) assigned app files to packages by path patterns. It did not compare implementations between apps or decide whose behaviour is the reference. That comparison is your job for one family of packages.

## App priority (whose behaviour is the reference when apps differ)

1. HeardRight: live, shipped to users.
2. ViewRight: live, shipped to users.
3. Pulse: used daily by the owner.
4. Everything else (CodeRight, GenRight, MailRight, ScrapeRight, CutRight, PostRight, Legion): to be decided. CutRight and PostRight were started just before the RightKit decision, so their code carries the least weight as a reference.

A package used by one app only still belongs in RightKit; judge it on whether a second app could adopt it without inheriting the first app's product decisions.

## Trees

S = `<scratch>`

App source as surveyed by the plan (about 1 Nov), plain extracted trees:
`S/survey/postright`, `S/survey/scraperight`, `S/survey/mailright`, `S/survey/viewright`, `S/survey/genright`, `S/survey/cutright`, `S/survey/pulse`. If `S/survey/coderight` exists, use it; if not, CodeRight source is not available and you say so. Legion source is not available.

Live builds: `S/survey/heardright-live` (Mac 0.1.213, 30 Sep), `S/survey/heardright-win-live/tauri-app-next` (Windows 0.1.162), `S/survey/viewright-live` (Mac 0.1.123, 19 Sep). HeardRight main today: `S/survey/heardright-main`.

Each tree has `docs/rightkit-inventory/<app>-rightkit-files.json`: `files[]` with `path`, `loc`, `target` (the planned RightKit package) and `action` (NEW, EXTEND, ADOPT, SHELL, APPTEST, DROP, ...). Filter by `target` to find the app-side code for a package. The mapping is path-based and sometimes wrong; search the code too.

RightKit: `/home/user/rightkit/crates/<name>`, `/home/user/rightkit/packages/<name>`, `/home/user/rightkit/swift/RightKitSwift/Sources/<Module>`. A planned package may exist as a family of crates (`rightkit-dictation` is `rightkit-dictation-*`), or under npm instead of cargo, or not at all.

Measured line overlap per file is in `S/coverage-rows.json` (array of `{app, path, loc, target, action, n, any, tgt}`: `n` distinctive lines, `any` found anywhere in RightKit, `tgt` found in the named package). Use it to find where to look, not as a verdict.

Earlier HeardRight reviews, if your family overlaps them: `S/hr-review-*.md`.

## What to decide for each package in your family

1. Does it exist in RightKit, and under what name(s)?
2. Which apps feed it according to the plan, with planned lines each.
3. Whose implementation is in the RightKit package today (which app's code, or a fresh implementation)?
4. Where two or more apps implement the same capability, read the implementations side by side and say how they differ in behaviour and API. Cite file:line.
5. The best shape: the public API and module boundaries the package should have so every feeding app can use it, taking the priority order above as the reference for behaviour. Say plainly whether today's package already is that shape.
6. Per app: can the app adopt the package as it is ("fits"), only after listed package changes ("gaps"), or would it have to give up behaviour it needs ("wrong")? "n/a" if the app has nothing for it.
7. Boundary problems: code in the package that is one app's product decision; code mapped here that belongs to another package; two RightKit packages holding the same code; a package that should be split or merged.
8. What must change in the package before it is published as the shape apps adopt (a short action list), or "none".

Depth: read real code on both sides for every package that two or more apps feed, and for every package over 5,000 planned lines. For small single-app packages a lighter read is fine; say which depth you used. Mark each finding as verified by reading or as a suspicion. Do not pad; "not verified" is an acceptable entry.

## Outputs

1. A Markdown report at the path given in your task: one section per package following the eight points, then a family summary.
2. A JSON file at the path given in your task: an array with one object per package:

```json
{
  "package": "rightkit-example",
  "kind": "cargo | npm | swift",
  "family": "your family name",
  "existsAs": ["crates/rightkit-example"],
  "feeds": {"heardright": 1200, "pulse": 300},
  "basis": "whose implementation the package holds today, in a few words",
  "referenceApp": "heardright",
  "shape": "best | gaps | wrong | missing",
  "shapeReason": "one or two sentences",
  "apps": {"heardright": {"fit": "fits | gaps | wrong | n/a", "note": "short"}},
  "boundary": ["short items, or empty"],
  "actions": ["short items the package needs before publication, or empty"],
  "depth": "deep | light",
  "confidence": "high | medium | low"
}
```

`shape` meanings: `best` = already the shape all feeding apps can adopt; `gaps` = right design, listed changes needed; `wrong` = boundaries or API should be redrawn; `missing` = planned but not in RightKit.

Return a summary of at most 25 lines: counts by shape, then the most important findings.
