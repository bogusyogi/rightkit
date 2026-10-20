# rightkit-http 0.3.7 registry consumer (PK12)

Standalone consumer, outside the workspace, pinning `rightkit-http =0.3.7` exactly with feature
`untrusted`. It exercises the untrusted web-text boundary: wrapping, injection signals, the
block-on-injection placeholder, and UTF-8-safe truncation.

State: in-memory strings only. No network, no files, no process spawn, no user data. Run only by the
CI consumer lane.

Not covered: the HTTP client, redirects, retries, quotas, SSE, multipart, secrets, and the async
client.
