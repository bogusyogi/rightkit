# Publishing credentials for RightKit

## How RightKit publishes today

Adrian, 2026-11-03 (rightkit#1471): "i want you to publish it from local. the npm and crates are both local. we will do a provenance skip
fuck your ci. feel free to push but no ci". RightKit's npm packages and crates publish from the Mac. Pushes to `main` carry `[skip ci]`.
npm provenance is off for owned packages (rightkit#1472). This replaces the CI-only route described further down and in `docs/CI.md`.

| Registry | Credential | Where it lives | Created by | Used by |
|---|---|---|---|---|
| npm (`@rightkit/*`) | Granular access token, `@rightkit` scope, publish rights, 2FA bypass | macOS Keychain, generic password `npm-rightkit-publish` (login keychain, account `adrdsouza`) | Adrian | `right-release publish-local npm` |
| crates.io | Registry token | macOS Keychain, through cargo's credential provider (`~/.cargo/config.toml`: `global-credential-providers = ["cargo:macos-keychain"]`; service `cargo-registry:https://github.com/rust-lang/crates.io-index`) | Adrian (`cargo login`) | `cargo publish` on the Mac |

Neither token is ever printed, passed on a command line or written to a file. The publisher reads the npm token inside its own process
and hands it to npm through a private temporary config; cargo reads its token from the Keychain itself.

### Who does what

- **Adrian** approves each release in chat: package, version and commit. He types nothing in a terminal; the npm token's 2FA bypass is what
  makes that possible. A plain `npm publish` under his own npm login needs his one-time code (his account requires 2FA for writes), so it
  is not the route.
- **The agent** builds the release commit, runs preflight and Class A on it, asks for approval with that output, pushes with `[skip ci]`,
  then runs the publisher.

### npm, step by step

```bash
node packages/release/cli/right-release.mjs publish-local npm --package @rightkit/<name> --commit <sha>
```

It refuses unless the commit is on `origin/main`, runs the gate's Class A for the package on that exact commit, and refuses a version
already on npm. It then exports the package folder from the commit, packs it, publishes with `--provenance=false`, and fails unless the
registry's shasum equals the packed tarball's. Check that the token still works, without publishing (prints the username and a count):

```bash
node packages/release/cli/right-release.mjs publish npm --check-auth
```

### Crates

The crates.io token is in place, and the local cargo route comes next (`publish-local cargo`, same steps: pushed commit, gate on that
commit, then `cargo publish` in dependency order, then a registry read-back). Until it lands, crates are published by hand on the Mac
after the gate, with Adrian's approval for each one.

## The CI route (not in use since 2026-11-03)

The rest of this file describes the CI route's GitHub secrets. They stay in place but no current publish reads them.

## Where the credentials are

| Registry | GitHub secret on `bogusyogi/rightkit` | Scope | Set | Source |
|---|---|---|---|---|
| npm (`@rightkit/*`) | `NPM_TOKEN` | `release` environment (main only) | 11 Oct 2026, 14:53 UTC, **wrong token: must be replaced** | First copy came from `~/.npmrc`, which is not a publish token: the first real publish (run 38157017970) was refused by npm with 404 on PUT. The publish token is the macOS keychain item `npm-rightkit-publish` that the old local publish path used. |
| crates.io | `CARGO_REGISTRY_TOKEN` | repository | 11 Oct 2026, 14:45 UTC | Token `rightkit-github-actions`, created by Adrian on crates.io with scopes `publish-new` and `publish-update`, no crate restriction |

Check that they exist (names and dates only; GitHub never returns a value):

```bash
gh secret list --repo bogusyogi/rightkit
```

```bash
gh secret list --env release --repo bogusyogi/rightkit
```

The crates.io token is repository-scoped, so any workflow in the repository can read it, not
only the publish job. Only Adrian's account can push to the repository, which bounds the risk.
Moving it into the `release` environment needs a new token from crates.io (the value cannot be
copied out of GitHub); do that at the next rotation, with `--env release` added to the command.

## Rules for every chat

1. If a publish fails for authentication, read the workflow log first. A missing or expired token
   shows as HTTP 401 or 403 from the registry. Anything else is not a credentials problem.
2. Never print, read back, or copy a token into a file, a chat, a commit, or a command argument.
   A value only ever travels through a pipe into `gh secret set`.
3. Publication is automatic from git; no chat dispatches it by hand. App chats ask the lead for a version;
   they do not publish and do not need the tokens.
4. A session whose guard hook refuses a credential read stops there. It does not route around
   the hook and does not ask another session to do the read.

## When a token really has expired

Only Adrian replaces a token. The whole job is:

1. **npm:** on npmjs.com create a granular access token with publish rights for the `@rightkit`
   scope, then run `gh secret set NPM_TOKEN --repo bogusyogi/rightkit` and paste it at the
   hidden prompt.
2. **crates.io:** Account Settings → API Tokens → New Token, scopes `publish-new` and
   `publish-update`, then run `gh secret set CARGO_REGISTRY_TOKEN --repo bogusyogi/rightkit`
   and paste it at the hidden prompt.
3. Update the "Set" column above in the same change.

The crates.io token's expiry date was chosen by Adrian when he created it and is not recorded
here. The first 401 from crates.io after a long quiet period most likely means it expired.

## What is not a credential question

- A crate or package that is not published yet is waiting on CI or review, not on a token.
  See `docs/CI.md`.
- Apple signing and Windows signing use different secrets, in each product repository's
  `release` environment. See `/Volumes/D/claude/docs/rules/release-signing.md`.
