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

## Credential posture (read before enabling package-manager publishing)

- **No code-signing credential ever reaches CI.** Signing and notarization stay local in `@rightkit/release`. The only publish credential in the npm lane is short-lived OIDC (trusted publishing, no long-lived token).
- **Exception, disclosed:** the optional, opt-in `publish-package-managers` lane *does* require two owner-provisioned, long-lived tokens in GitHub Actions `secrets.*` (`HOMEBREW_TAP_TOKEN`, `WINGET_CREATE_GITHUB_TOKEN`) to open tap/manifest PRs. These are real bearer tokens, public-repo-scoped by the visibility gate but not zero-exposure. The "no signing credential in CI" invariant is specifically about code-signing material; it does not mean "no credential of any kind."

Manifest input is treated as untrusted: every field flowing into rendered YAML is charset-allowlisted, so a manifest cannot inject `${{ secrets.* }}` or shell/expression syntax.

## Install

```sh
npm install -g @rightkit/git   # or: npx @rightkit/git --help
```

Node builtins only; no runtime dependencies.

## Profiles

`profile` defaults to `node`. `rust-hybrid` accepts only `ci`, requires safe relative `gate.path`, installs pinned Node plus `rustup stable`, then runs that gate. It never adds release, signing, publishing, or approval steps.
