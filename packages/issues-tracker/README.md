# @rightkit/issues-tracker

`right-issues` writes `issues.html` at a repository root from ALL of that repository's GitHub issues, open and closed: one self-contained, offline page (no network, light and dark) that shows what needs the owner first, then every issue grouped by feature or component. Plain `.mjs`, no build, zero runtime dependencies; it needs the GitHub CLI (`gh`) unless `--issues` is given.

| Item | Value |
|---|---|
| Where it runs | Every repository (agent and developer tooling) |
| Rust pair | None |
| Version | 0.1.0, staged and never published; see the `@rightkit/issues-tracker` row in [INDEX.md](../../INDEX.md) and `issues-tracker` in [docs/CAPABILITY-MAP.md](../../docs/CAPABILITY-MAP.md) |
| License | MIT OR Apache-2.0 |

## Commands

```
pnpm exec right-issues [--root <dir>] [--repo <owner/name>] [--out <file>] [--groups <file>]
                       [--issues <file>] [--who <name>] [--closed-days <n>] [--max <n>]
right-issues --version | --help
```

- `--root` repository root (default: current directory); `--repo` defaults to `gh repo view` in the root.
- `--out` output file (default `<root>/issues.html`); `--groups` groups file (default `<root>/.rightkit/issue-groups.json`, optional).
- Every issue is fetched through the paginated REST API (`gh api --paginate --slurp repos/<repo>/issues?state=all&per_page=100`), pull requests are dropped, and the count is checked against the search total. If retrieval fails or is incomplete, nothing is written (an existing `issues.html` stays as it was) and the exit code is non-zero.
- All local arguments are validated before `gh` is called.
- `--issues` reads the issue list (GitHub REST shape, or `number,title,state,labels,updatedAt,closedAt,url,body`) from a file instead of calling `gh`.
- `--who` names who decisions go to, shown as "Needs <name>" (default `Adrian`).
- `--closed-days` sets what the default view ("Open + recent") shows: open issues plus issues closed in the last n days (default 14; a closed issue with no `closedAt` ages from its last update). Every closed issue stays in the page; the "Done" and "All" filters show the full history.
- `--max` refuses (writes nothing) when the repository has more than n issues. There is no cap by default and no silent truncation.

The page lists a "Needs <who>" section (decisions, then live checks, with the first paragraph of the issue body as the question), a count strip, search, status filters, and a collapsible card per group with counts.

## Status labels

Status is read from labels the agents set; nothing is guessed. When several apply, the first row wins. A closed issue is always `done`; an issue with none of these is `open`.

| Chip | Labels |
|---|---|
| Needs decision | `needs-decision`, `needs-adrian` |
| Live check | `needs-live-run`, `needs-live-check` |
| Built, needs check | `built-needs-check`, `fixed-unverified` |
| Blocked | `blocked`, `blocked-upstream` |
| Tracking | `tracking` |

## Groups

An issue's group is the first of: the `issues` map in the groups file, a label with a group prefix, a title `rules` match, else `Other`. Group label prefixes (case-insensitive) in priority order: `group:`, `component:`, `feature:`, `area:` (for example `area:Hub` puts the issue in "Hub"). When an issue carries several group labels, the highest-priority prefix wins and ties break by sorted label name, so label order never changes the group. Group names are trimmed and compared case-insensitively; the first declared spelling (in `groups`) wins, else the lexically smallest.

`.rightkit/issue-groups.json`, all keys optional:

```json
{
  "groups": ["Hub", "Voice", "Release"],
  "issues": { "812": "Hub" },
  "rules": [["Voice", ["\\bvoice\\b", "\\bmic(rophone)?\\b"]]]
}
```

- `groups` sets the order of the cards; duplicates (case-insensitive) collapse to the first spelling. Groups not listed follow alphabetically, `Other` last.
- `issues` maps an issue number to a group name.
- `rules` is a list of `[group, [regex, ...]]`, matched case-insensitively against the title. An invalid pattern fails with the rule's name.

## Per-repo rule

Each repository commits its own `issues.html` (and groups file), regenerated with `pnpm exec right-issues` in the same commit whenever an agent files, closes or relabels an issue, and at least once per working session. There is no shared copy and no per-app generator.
