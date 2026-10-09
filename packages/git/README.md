# @rightkit/git

Public-repository-only GitHub Actions workflow layer for Right Suite products. The CLI `right-git` renders and installs CI lanes into `.github/workflows/` from a `.rightgit.json` manifest.

| Item | Value |
|---|---|
| Where it runs | Build and release tooling (repository CI setup) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Commands

- `right-git install <repo> --lanes ci,...` writes `.rightgit.json` and renders workflows.
- `right-git sync` re-renders installed workflows from the manifest.
- `right-git drift --all` checks installed workflows against templates and re-checks visibility.
- `right-git adopt` imports existing workflows into the template library.
- `right-git uninstall` removes every managed file.

`install` and `sync` refuse private, internal and unconfirmed-visibility repositories. There is no override flag.

## Optional QA evidence upload (rust-hybrid)

Set `qaEvidencePath` in `.rightgit.json` to upload the gate's QA output as a workflow artifact. The path is relative to `$RUNNER_TEMP`; the rendered step uses `${{ runner.temp }}/<path>`. Absolute paths and expression syntax are rejected.

```json
{
  "profile": "rust-hybrid",
  "gate": { "path": "scripts/gate.sh" },
  "qaEvidencePath": "pulse-hub-qa",
  "qaEvidenceRetentionDays": 14
}
```

- The step runs after the gate with `if: always()` and `if-no-files-found: ignore`.
- Artifact name: `qa-evidence-${{ matrix.os || runner.os }}-${{ github.run_attempt }}`.
- `qaEvidenceRetentionDays` is an integer from 1 to 90 and defaults to 7. It requires `qaEvidencePath`.
- Omit the key and the rendered `ci.yml` is unchanged.

## Full validation on every OS (rust-hybrid)

Set `"ciAllOs": true` to run the rust-hybrid `ci` gate on every `matrix.os` runner instead of one. Runners run in declared order with `fail-fast: false`. Each leg uploads its own artifact, because the QA artifact name already includes the runner label.

```json
{
  "profile": "rust-hybrid",
  "matrix": { "os": ["xcode-27", "windows-2025"], "node": ["26.8.1"] },
  "ciAllOs": true
}
```

Omit the key, or set it to `false`, and the rendered `ci.yml` keeps one runner, preferring the first `windows-*` entry. `ciAllOs: true` is rejected for other profiles.

## Entry points

No importable API. The package has no `main` and no `exports` map. The supported surface is the `right-git` bin.

## Peer dependencies

None. No runtime dependencies (Node builtins only).

## Example

```sh
pnpm add -D @rightkit/git
pnpm exec right-git install . --lanes ci
pnpm exec right-git drift --all
```

## Known issues

- The optional `publish-package-managers` lane needs two long-lived tokens in GitHub Actions secrets. Their names and scope are in the package templates, and they are not zero-exposure. Read `docs/rules/github-access.md` in the workspace before enabling it.
- Manifest fields are charset-checked before they reach workflow YAML.
