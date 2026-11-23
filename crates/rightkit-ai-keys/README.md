# rightkit-ai-keys

Bring-your-own provider key management over `rightkit-credentials::slots` & `rightkit-http`. `KeyVault` reads, saves, clears & attempts every requested slot change. `validate_key` trims input & requires 12 bytes; `apply` validates saves while `save` accepts already validated input. `classify_key_read` preserves present, absent & read-error states.

Host supplies credential-store opener, namespace, account IDs, provider labels, aliases, HTTPS endpoints, HTTP client factory, entitlement checks & readiness policy. Store opener errors must contain safe diagnostics only. Each storage operation opens one slot store; retrying reads delegate one 100 ms retry to credentials. Per-slot failures remain visible & never stop subsequent changes.

`check_key_live` validates configured public HTTPS endpoint without userinfo before attaching secret Authorization. HTTP's built-in redirect policy permits only that initial origin; request timeout is five seconds. Blank or control-bearing keys are rejected without transport. Status 2xx is Valid, 401/403 Rejected, other statuses or transport failures Unverified. Invalid configuration returns static error text. Request errors, response bodies & key values are never logged or returned in diagnostics.

`rightkit-ai-accounts` owns account discovery & switching; `rightkit-llm` owns runtime credential lookup. This crate coordinates optional key settings without replacing either owner. Hosts consume published crate versions.
