# @rightkit/git

Public-repo-only GitHub Actions workflow templates for the Right/Orthic product suite. One CLI, `right-git`, that renders and installs CI lanes into a repository — and refuses to touch a private one.

## Why public-only

GitHub Actions minutes are free and unlimited on public repos, and billed on private ones. `@rightkit/git` enforces that boundary in code, not by convention:

1. **Install-time refusal** — `right-git install`/`sync` resolves repo visibility and refuses any private or internal repo, with no override flag. Visibility fails closed: an unconfirmed repo is treated as not-public.
2. **Run-time guard** — every rendered workflow carries an unconditional `private-repo-guard` job that fails loudly (`exit 1`) if the repo is ever flipped private after install; every other job depends on it.
3. **Drift audit** — `right-git drift --all` re-checks visibility across managed repos.

## What it does

- **`right-git install|sync`** — render CI lanes (ci, qualification, package-smoke, release-candidate, publish-npm, publish-package-managers) from a `.rightgit.json` manifest into `.github/workflows/`.
- **`right-git drift`** — detect divergence between installed workflows and templates, and orphaned managed workflows.
- **`right-git adopt`** — import existing workflows into the template library.
- **`right-git uninstall`** — remove every managed file, leaving zero behind.

Workflows are thin callers over the repo's own scripts, so a private repo running the same gates locally executes identical checks.

Sync records exact prior bytes, refuses unmanaged same-name workflows, & preflights every target before any write. Qualification preserves schema logs with a unique required artifact.

## Credential posture (read before enabling package-manager publishing)

- **Signing credentials stay environment-scoped.** Opt-in Windows candidates use GitHub OIDC with Azure Artifact Signing. Opt-in macOS candidates use a temporary runner keychain plus Apple API-key notarization. Signing jobs run only for `v*` tag refs; manual dispatch produces unsigned candidates only. npm publishing uses short-lived OIDC.
- **Exception, disclosed:** the optional, opt-in `publish-package-managers` lane *does* require two owner-provisioned, long-lived tokens in GitHub Actions `secrets.*` (`HOMEBREW_TAP_TOKEN`, `WINGET_CREATE_GITHUB_TOKEN`) to open tap/manifest PRs. These are real bearer tokens, public-repo-scoped by the visibility gate but not zero-exposure. The "no signing credential in CI" invariant is specifically about code-signing material; it does not mean "no credential of any kind."

Manifest input is treated as untrusted: every field flowing into rendered YAML is charset-allowlisted, so a manifest cannot inject `${{ secrets.* }}` or shell/expression syntax.

## Install

```sh
npm install -g @rightkit/git   # or: npx @rightkit/git --help
```

Node builtins only; no runtime dependencies.

## Profiles

`profile` defaults to `node`. `rust-hybrid` requires `ci`, accepts optional `release-candidate`, installs pinned Node plus Rust, then runs product-owned package scripts. Candidate configuration is `{ buildScript, checkScript, artifactRoot, os, targets?, signWindows?, signMacos?, releaseChain? }`. Legacy `os` remains supported & derives one target per runner; explicit `targets` is `{ os, platform, architecture }[]`, independent of main CI matrix. Each target maps to native runner, Rust target triple, exact unsigned artifact, & platform signing matrix. Supported platform values are `windows`, `macos`, `linux`; architectures are `x86_64`, `arm64`. RightGit always uploads an unsigned handoff. `signWindows: true` adds Azure OIDC signing. `signMacos: true` adds Developer ID signing plus notarization from protected environment secrets.

`releaseChain` lets RightGit own full protected DAG while product scripts own artifact semantics: `{ windowsFinalizeScript, macosFinalizeScript?, installedQualificationScript, publishScript, qualificationRunnerVariable, publishSecrets? }`. Generated ordering is unsigned candidate → platform signing/finalization → installed qualification → publication. It deliberately rejects more than one Windows target or more than one macOS target because its installed-qualification/publish handoff has one canonical signed artifact per platform. Every protected job is tag-gated, publication needs successful installed qualification plus macOS finalization when enabled, & only publish job receives declared publication secrets plus `contents: write`.
