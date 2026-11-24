# rightkit-google-apis

Async Google Workspace REST clients: Gmail (sync, history, batch fetch, MIME, compose, labels, filters, settings, drafts, attachments), Calendar, Drive, People, Tasks and Meet, with desktop OAuth and shared retry/backoff.

## Tokens and stores

`Google::new(client_id, client_secret, refresh_token)` keeps the refresh token in memory. `Google::with_store(client_id, client_secret, account, store)` instead takes the grant from a caller-supplied `rightkit_oauth::TokenStore`: access tokens are minted from it and refreshed back into it, and the refresh token never appears in this crate's arguments, logs, errors or `Debug` output. An empty or revoked grant is `GoogleError::Auth` ("sign in again"), never a retry loop.

Feature `sealed-store` (off by default) adds `oauth::sealed_file_store(root, key)`, an encrypted file store over `rightkit-oauth`'s `SealedFileTokenStore` for Unix daemons (it fails closed on Windows). The 32-byte `key` is the caller's to provide from a credential its service manager supplies, never from a file beside `root`. `oauth::provision_refresh_token(store, account, refresh_token)` puts a sign-in's refresh token into a store.

Gmail's `&Google` functions (`gmail::list_message_page`, `gmail::fetch_sync_with`, `run_sync_pass`, and the rest) and Calendar's `list_calendars_with` and `list_events_with` take a client built either way. Calendar reads follow `nextPageToken` up to a caller-set `PageCap`, and report `truncated` when the cap stopped them.

Moved into RightKit from MailRight (mailright src-tauri google, gmail, calendar, drive, people, tasks, meet, labels, filters, mail_settings, drafts, attachments, oauth) on 2026-11-01.
