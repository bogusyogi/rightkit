# rightkit-http

Shared blocking HTTP client for Right Suite applications. It provides timeouts, bounded bodies, retry/backoff that honors `Retry-After` (never retrying unsafe methods), streaming responses, WHATWG Server-Sent Events decoding, redacted secrets, and tamper-evident wrapping of untrusted web text before it reaches a model.

The `testing` feature enables an in-process mock server that records exact requests.

Publish through the RightKit release workflow.
