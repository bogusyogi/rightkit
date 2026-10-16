# rightkit-agent-tools: agent demo contract (v1)

Status: v1, feature `demo` of `rightkit-agent-tools` (non-default). The Rust types are in
`src/backends/demo.rs`; this document is the wire contract for CutRight's TypeScript binding,
which derives from it. The Rust types are the source of truth; the JSON Schema below mirrors them.

## Scope and gating

- Compiled only with `--features demo`. A default or release build of `rightkit-agent-tools`
  contains no demo code, no demo tool names, and no demo service name. In a default build
  `right-ctl demo ...` is an ordinary app-name route; in a demo build `demo` is the demo subcommand.
- The fixture app `app.orthiclabs.demo-fixture` (`FIXTURE_APP_ID`) is defined only under this
  feature. It is never a production catalog entry.
- The host (CutRight) owns consent, the session state machine, policy, capture and the bundle.
  This contract is the client surface only. The agent never drives an app directly.
- Surface: `right-ctl demo <tool> [key=value | JSON object]` and the `demo.*` tools in a Toolbox.

## Transport

- Daemon service: `rightkit.agent.demo` (`rightkit-daemon`, the user-only socket or pipe).
- Method names equal tool names: `demo.request`, `demo.apps`, `demo.snapshot`, `demo.act`,
  `demo.batch`, `demo.end`.
- Client app label: `right-ctl`.
- Params and results are JSON objects matching the schemas below.
- Timeouts: `demo.request` 600 s (it waits for a person); every other call 30 s.

## Session and token

1. `demo.request {app, agent_name}` blocks until the person answers the consent dialog.
   Approval mints a session and returns `{session, token, app}`. Denial returns
   `consent_denied` and no session exists.
2. Every later call except `demo.apps` carries `token`. An unknown token returns
   `invalid_token`.
3. Consent is per session and is never remembered. A new session requires a new
   `demo.request` and a new consent.
4. `demo.end` seals the bundle and invalidates the token. Any later call with that token
   returns `invalid_token`, or `session_ended` when the host can tell it was this session.
5. The kill control ends the session with `bundle_state: "killed"`. Later calls return
   `session_killed`.
6. The client stores nothing. The token is an argument on each call. `right-ctl` never
   writes it to disk.

## Capture-safe rule

A target that cannot report a capture-safe state is refused, never masked. `demo.snapshot`
returns `capture_unsafe` in that case, and no element is listed. The only success value is
`capture_state: "safe"`. The client also fails closed: a snapshot whose `capture_state` is
anything other than `safe` becomes `capture_unsafe`, and no element is returned.

## CLI argument rules

- `right-ctl demo <tool>` takes one JSON object, or `key=value` pairs.
- The text keys `token`, `ref`, `text`, `key`, `app` and `agent_name` keep their value verbatim.
  `text=1e3` types `1e3`. Other values parse as JSON when they are JSON, else stay strings.

## Error codes

Each code is a `ToolError.code` string. `retryable` comes from the host.

| code | meaning |
| --- | --- |
| `demo_disabled` | Demo mode is switched off in the host. |
| `consent_denied` | The person refused consent. Nothing ran. |
| `invalid_token` | Token missing, unknown, or from an ended session. |
| `session_ended` | The session ended normally. |
| `session_killed` | The session ended by the kill control. |
| `unknown_app` | The app id is not in the demo catalog. |
| `not_armed` | Consent given, but the session is not armed yet. |
| `permission_missing` | A required OS permission is not granted. |
| `target_blocked` | A host gate refused the target: a secure or credential field, a denied app, or the host itself. |
| `capture_unsafe` | The target cannot report a capture-safe state. Refused, not masked. |
| `invalid_arguments` | Arguments fail the contract (client-side or host-side). |
| `service_unavailable` | The host's demo service is not running. |
| `malformed_response` | The host answered with a shape this contract does not allow. |

Unknown codes from the transport (for example `not_allowed` from the daemon) pass through
unchanged.

## Limits

- `demo.batch` takes 1 to 20 steps. The client rejects other counts before connecting.
- `demo.batch` stops at the first failing step. It returns success with `failed_step` and
  `error_code` set, as `right-ctl <app> batch` does.

## JSON Schema

Draft 2020-12. Each `$defs` entry is one shape. The tool input schemas are the `params` entries.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "rightkit.agent.demo.v1",
  "$defs": {
    "token": {"type": "string", "description": "Opaque session token from demo.request."},
    "elementRef": {"type": "string", "description": "Valid only for the session that issued it."},
    "elementOp": {"type": "string", "enum": ["click", "type", "press"]},
    "step": {
      "type": "object",
      "properties": {
        "ref": {"$ref": "#/$defs/elementRef"},
        "op": {"$ref": "#/$defs/elementOp"},
        "text": {"type": "string", "description": "Required for type."},
        "key": {"type": "string", "description": "Required for press, e.g. enter or cmd+s."}
      },
      "required": ["ref", "op"]
    },
    "errorCode": {
      "type": "string",
      "enum": [
        "demo_disabled", "consent_denied", "invalid_token", "session_ended", "session_killed",
        "unknown_app", "not_armed", "permission_missing", "target_blocked", "capture_unsafe",
        "invalid_arguments", "service_unavailable", "malformed_response"
      ]
    },
    "request": {
      "params": {
        "type": "object",
        "properties": {"app": {"type": "string"}, "agent_name": {"type": "string"}},
        "required": ["app", "agent_name"]
      },
      "result": {
        "type": "object",
        "properties": {
          "session": {"type": "string"},
          "token": {"$ref": "#/$defs/token"},
          "app": {"type": "string"}
        },
        "required": ["session", "token", "app"]
      }
    },
    "apps": {
      "params": {"type": "object", "properties": {}},
      "result": {
        "type": "object",
        "properties": {
          "apps": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {"id": {"type": "string"}, "name": {"type": "string"}},
              "required": ["id", "name"]
            }
          }
        },
        "required": ["apps"]
      }
    },
    "snapshot": {
      "params": {
        "type": "object",
        "properties": {"token": {"$ref": "#/$defs/token"}},
        "required": ["token"]
      },
      "result": {
        "type": "object",
        "properties": {
          "capture_state": {"type": "string", "enum": ["safe"]},
          "elements": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "ref": {"$ref": "#/$defs/elementRef"},
                "role": {"type": "string"},
                "label": {"type": "string"},
                "ops": {"type": "array", "items": {"$ref": "#/$defs/elementOp"}}
              },
              "required": ["ref", "role", "label", "ops"]
            }
          }
        },
        "required": ["capture_state", "elements"]
      }
    },
    "act": {
      "params": {
        "type": "object",
        "properties": {
          "token": {"$ref": "#/$defs/token"},
          "ref": {"$ref": "#/$defs/elementRef"},
          "op": {"$ref": "#/$defs/elementOp"},
          "text": {"type": "string"},
          "key": {"type": "string"}
        },
        "required": ["token", "ref", "op"]
      },
      "result": {
        "type": "object",
        "properties": {"receipt_id": {"type": "string"}},
        "required": ["receipt_id"]
      }
    },
    "batch": {
      "params": {
        "type": "object",
        "properties": {
          "token": {"$ref": "#/$defs/token"},
          "steps": {"type": "array", "items": {"$ref": "#/$defs/step"}, "minItems": 1, "maxItems": 20}
        },
        "required": ["token", "steps"]
      },
      "result": {
        "type": "object",
        "properties": {
          "completed": {"type": "integer", "minimum": 0},
          "results": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {"receipt_id": {"type": "string"}},
              "required": ["receipt_id"]
            }
          },
          "failed_step": {"type": ["integer", "null"], "minimum": 0},
          "error_code": {"oneOf": [{"$ref": "#/$defs/errorCode"}, {"type": "null"}]}
        },
        "required": ["completed", "results", "failed_step", "error_code"]
      }
    },
    "end": {
      "params": {
        "type": "object",
        "properties": {"token": {"$ref": "#/$defs/token"}},
        "required": ["token"]
      },
      "result": {
        "type": "object",
        "properties": {
          "ended": {"type": "boolean", "const": true},
          "bundle_id": {"type": "string"},
          "bundle_state": {"type": "string", "enum": ["complete", "killed"]}
        },
        "required": ["ended", "bundle_id", "bundle_state"]
      }
    }
  }
}
```
