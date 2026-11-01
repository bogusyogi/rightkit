# @rightkit/waitlist-worker

Cloudflare Worker that captures waitlist email signups in KV, rate-limits per IP and sends an
optional Resend confirmation email.

| Item | Value |
|---|---|
| Where it runs | Cloudflare Workers |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Use

```ts
import { createWaitlistWorker } from "@rightkit/waitlist-worker";

export default createWaitlistWorker({
  allowedOrigins: ["https://example.com"],
  detailFields: [{ name: "current_tool", maxLength: 80 }],
  email: { from: "Example <hello@example.com>", subject: "You're on the waitlist", text: "…", html: "…" },
});
```

`example/` holds a complete worker entry and `wrangler.toml`.

## Bindings and secrets

| Name | Kind | Purpose |
|---|---|---|
| `WAITLIST` | KV (required) | Signups keyed by lower-cased email. Source of truth. |
| `RATELIMIT` | KV (optional) | Per-IP counter, `rateLimitPerHour` (default 10) per hour. |
| `RESEND_API_KEY` | secret (optional) | Sends the confirmation email; without it no email is sent. |
| `IP_HASH_SECRET` | secret (optional) | Stores client IPs as an HMAC; without it a truncated SHA-256. |
| `CF_DEV` | var (optional) | `"1"` allows any `http://localhost:*` origin. |

## Responses

`{ ok: true }` for a new signup; `{ ok: true, already_subscribed: true }` for a duplicate, with
`updated: true` when detail fields were merged; `{ error }` with `invalid_email`, `invalid_json`,
`rate_limited` or `method_not_allowed`.

## Testing

```sh
pnpm --dir packages/waitlist-worker test
```
