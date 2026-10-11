# Publishing credentials for RightKit

**Settled on 11 October 2026 by Adrian. Do not reopen, do not ask him for these tokens again.**

RightKit (`Orthic-Labs/rightkit`, public) publishes npm packages and crates from GitHub Actions.
No machine publishes locally, and no chat needs a registry token in its shell.

## Where the credentials are

| Registry | GitHub secret on `Orthic-Labs/rightkit` | Scope | Set | Source |
|---|---|---|---|---|
| npm (`@rightkit/*`) | `NPM_TOKEN` | `release` environment (main only) | 11 Oct 2026, 14:53 UTC | Copied by pipe from the Mac's npm login |
| crates.io | `CARGO_REGISTRY_TOKEN` | repository | 11 Oct 2026, 14:45 UTC | Token `rightkit-github-actions`, created by Adrian on crates.io with scopes `publish-new` and `publish-update`, no crate restriction |

Check that they exist (names and dates only; GitHub never returns a value):

```bash
gh secret list --repo Orthic-Labs/rightkit
```

```bash
gh secret list --env release --repo Orthic-Labs/rightkit
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
3. Only the RightKit lead dispatches the publish workflow. App chats ask the lead for a version;
   they do not publish and do not need the tokens.
4. A session whose guard hook refuses a credential read stops there. It does not route around
   the hook and does not ask another session to do the read.

## When a token really has expired

Only Adrian replaces a token. The whole job is:

1. **npm:** on npmjs.com create a granular access token with publish rights for the `@rightkit`
   scope, then run `gh secret set NPM_TOKEN --repo Orthic-Labs/rightkit` and paste it at the
   hidden prompt.
2. **crates.io:** Account Settings → API Tokens → New Token, scopes `publish-new` and
   `publish-update`, then run `gh secret set CARGO_REGISTRY_TOKEN --repo Orthic-Labs/rightkit`
   and paste it at the hidden prompt.
3. Update the "Set" column above in the same change.

The crates.io token's expiry date was chosen by Adrian when he created it and is not recorded
here. The first 401 from crates.io after a long quiet period most likely means it expired.

## What is not a credential question

- A crate or package that is not published yet is waiting on CI or review, not on a token.
  See `docs/CI.md`.
- Apple signing and Windows signing use different secrets, in each product repository's
  `release` environment. See `/Volumes/D/claude/docs/rules/release-signing.md`.
