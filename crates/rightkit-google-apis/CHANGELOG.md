# Changelog

## 0.2.3

- Added `calendar::create_event_with(g: &Google, calendar_id: &str, ev: EventInput) -> Result<CalEvent, String>` (additive): `create_event` through an authorised client such as `Google::with_store`, with no refresh token. The same request, `EventInput` and `CalEvent`; `create_event` stays and now calls it. The request sends `sendUpdates=all`, so Google emails any attendee named in `ev.attendees`. Written, not compiled.

## 0.2.2

- Added authorised-client variants for sending (AG-6), additive: `gmail::send_message_with(g: &Google, raw_mime: &str, thread_id: Option<&str>) -> Result<SentRef, String>`, `drafts::create_draft_with(g: &Google, raw_mime: &str, thread_id: Option<&str>) -> Result<(String, Option<String>), String>` and `gmail::modify_labels_with(g: &Google, ids: &[String], add: &[&str], remove: &[&str]) -> Result<(), String>`. They take a `Google` (for example `Google::with_store`) instead of a client id, secret and refresh token; the requests, error text and the uncertain-send classification are those of `send_message`, `create_draft` and `modify_labels`, which stay and now call them. Written, not compiled. No unit tests, per the 2026-11-03 rule.

## 0.2.1

- Added `Google::with_store(client_id, client_secret, account, store)`: a client whose refresh token lives in a caller-supplied `rightkit_oauth::TokenStore` instead of being passed as a string. Access tokens are minted from, and refreshed back into, that store; a missing or revoked grant surfaces as `GoogleError::Auth`, with no re-seeding and no retry loop. Clones share one token holder. `Google::new` and every string-taking function are unchanged.
- Added `oauth::AccessTokens::with_store`, `oauth::provision_refresh_token(store, account, refresh_token)` (writes a sign-in's refresh token into a store as an expired grant) and, behind the new `sealed-store` feature, `oauth::sealed_file_store(root, key)` over `rightkit-oauth`'s `SealedFileTokenStore`. The 32-byte key is the caller's to supply from a service-manager credential; nothing here reads a key from disk.
- Added `gmail::fetch_sync_with(&Google, start_history_id)`, `fetch_sync` over a caller-built client; it is now re-exported from `gmail` (the function existed in the private `sync_full` module but was unreachable).
- Calendar reads now follow `nextPageToken`. Added `calendar::list_calendars_with` and `calendar::list_events_with` (over a `&Google`, returning `Paged<T>` with a `truncated` flag), `calendar::PageCap { max_pages, max_items }` (default 40 pages, 10,000 items) and `calendar::Paged`. `list_calendars` and `list_events` keep their signatures and now return every page up to the default cap; before, they returned only the first page (up to 250 and 2,500 items). `list_events` now escapes the calendar id in the URL path (an id containing `#` used to cut the request short).
- `Google` now implements `Debug`, printing `Google { [redacted] }`.
- New feature `sealed-store` (off by default), enabling `rightkit-oauth/server`.
- Known limits: `SealedFileTokenStore` fails closed on Windows (the intended host is Linux); the resolver deletes the stored grant on `invalid_grant`; a sealed-store failure surfaces as `GoogleError::Parse`, not a store-specific variant.

## 0.2.0

- Requires `rightkit-http` 0.4.0 (a breaking release), so this release is breaking too.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from MailRight (mailright src-tauri google, gmail, calendar, drive, people, tasks, meet, labels, filters, mail_settings, drafts, attachments, oauth).
