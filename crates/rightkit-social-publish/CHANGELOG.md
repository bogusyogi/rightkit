# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-logs` 0.3 -> 0.4. 0.1.3 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.3 in place (#1572). No source change in this crate since 0.1.3.

## 0.1.3 (unpublished)

- Fixed (#1496, part): `readiness_of` reads `token_expires_at`. A token at or past its expiry blocks readiness with the existing `reauth_required` code (the blocker vocabulary is unchanged). An account already `reauth_required` reports it once.
- Fixed (#1496): a step that ends `token_expired` (a 401 no refresh could repair) now calls the new `MachineStore::set_reauth_required(scope, platform)` in the transaction that stores the job outcome; reconcile reads that meet a 401 call it too. Decision: a DEFAULT no-op method, not a required one, so existing implementers still compile (no downstream implementer of `MachineStore` was found in postright or mailright); a host with an account table overrides it.
- Added (#1496): OAuth refresh in `refresh` for X (`/2/oauth2/token`), TikTok (`/v2/oauth/token/`), Pinterest (`/v5/oauth/token`, Basic client auth) and YouTube (token host from the `youtube_token` endpoint), driven by the new `Drive::oauth` (`OAuthRefresh`); the step is repeated once after a successful refresh (a 401 means nothing was applied); a rotated refresh token is returned by `Drive::take_rotated_refresh`. `Drive` gained the public fields `oauth` and `rotated_refresh` (hosts that build `Drive` literally add `oauth: None, rotated_refresh: Default::default()`). Not refreshed, with the reason recorded in `refresh.rs`: Threads and Instagram (refresh needs an unexpired token), Facebook (Page tokens do not expire), Mastodon (no refresh token), LinkedIn (programmatic refresh is for approved partners only). The mock-platform failure scenarios named in the issue are not added: no mocks, end-to-end coverage only.
- Added (#1497): the delayed `comment` phase. `PublishPayload::first_comment` (optional) and `PublishRemote::comment_id`; after `done` the job moves to phase `comment` (state stays `published`, delay `settings.first_comment_delay_secs`, default 60 s) and a step sends the follow-up: X (`reply.in_reply_to_tweet_id`), Facebook and Instagram (`POST /<id>/comments`), Mastodon (`in_reply_to_id` with `Idempotency-Key <job key>:comment`), LinkedIn (`/rest/socialActions/<urn>/comments`). A lost answer is `outcome_unknown` at the phase; `publish.reconcile` repeats Mastodon's idempotent request, reads the comments on Facebook, Instagram and LinkedIn and adopts the one match, and leaves X for `publish.resolve_unknown`, which now also closes a comment-phase job (`posted` with the comment id, or `not_posted`). `comment::check` validates a payload at enqueue. A comment that cannot be posted ends the phase with outcome `comment_failed` and never changes the published post. Unsupported (refused, recorded in `comment.rs`): Pinterest, TikTok, YouTube, Threads, Bluesky. Endpoints are the documented ones (fetched 2026-11-03), unverified against live accounts.

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Added `protocol` (channel `Scope` and the publish job and command payloads), `calendar_time`, `util` and `quota`: per-account pacing and the `next` selector over a host-implemented `PublishQuotaStore`. Additive.
- Added `machine` (`MachineStore`: the phase machine, `step_once` and `reconcile_once`), `deletion` (`DeletionStore`; media removal via rightkit-fs), `accounts` (`AccountStore`), `util::WriteTxn`, `jobs::PublishJobStore` (the host's job table: load, save, transaction, role gate) and `resolve` (`publish.resolve_unknown`).

## 0.1.0 (unreleased)

- First RightKit release, moved from PostRight (postright-engine publish adapters, http, media, platform, api_versions; postright-protocol publish payload).
