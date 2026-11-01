# CodeRight reference workflow format (v1)

This document defines the canonical YAML/JSON authoring format used by
every reference workflow in this fixture directory. It is the
single normative source for v1 integrators and for the dogfood round-trip
test that compiles each fixture into a `CompiledPlan` via
`WorkflowDefinition → compile(...)` in
`engine/crates/workflow/src/compiler.rs`.

The authoritative Rust type names live in
`engine/crates/workflow/src/domain.rs` (`NodeKind`, `Capability`,
`CapabilitySet`, `CompiledPlan`, `WorkflowDefinition`, `WorkflowVersion`).
The JSON shape described here is what the compiler produces when it
canonicalizes a fixture before hashing.

## Top-level keys

Every reference workflow MUST declare all of:

| Key | Type | Required | Description |
|---|---|---|---|
| `kind` | string const `"coderight-workflow/v1"` | yes | Authoring format version marker. |
| `schema_version` | integer, must be `1` | yes | Schema version. Bumped on breaking changes. |
| `id` | string, pattern `wfd-<kebab-name>` | yes | Deterministic, human-meaningful workflow definition id. |
| `name` | string | yes | Short human title. |
| `description` | string | yes | One-paragraph description. |
| `trigger` | trigger object | yes | Trigger surface (manual, schedule, event, api, or combined). |
| `inputs` | map of input spec | yes | Declared inputs and defaults. |
| `capabilities` | list of capability strings | yes | Workflow-level effective capability ceiling. |
| `nodes` | list of node specs | yes | Ordered node graph. |
| `edges` | list of `{ from, to, when? }` | yes | Directed edges; `from` may be a node id, `"start"`, or `"end"`. |
| `budgets` | budget object | yes | Hard ceilings for cost, tokens, wall time, and depth. |

Additional keys are rejected at compile time (`UnknownField`).

## Trigger shapes

Exactly one of the following trigger variants is required.

### `manual`

```yaml
trigger:
  manual:
    initiator_kinds: ["user", "desktop"]
```

Manual runs originate from an authenticated operator action through the
daemon API or the desktop UI. `initiator_kinds` constrains which surfaces
may fire the trigger.

### `schedule`

```yaml
trigger:
  schedule:
    cron: "0 7 * * *"
    tz: "UTC"
```

A cron schedule with an explicit IANA timezone. DST handling, misfire
policy, and overlap policy are enforced at compile time.

### `event`

```yaml
trigger:
  event:
    kind: "vendor.message_received"
    filter: "input.labels includes 'auto-triage'"
```

A normalized internal event with an optional expression filter. The
filter is evaluated against the trigger payload and must use the
expression syntax described below.

### `api`

```yaml
trigger:
  api:
    path: "/internal/morning-briefing"
    method: "POST"
```

Authenticated HTTP endpoint. Admission performs provenance, dedupe, and
trust checks before normalizing the event.

### `manual_and_event`

```yaml
trigger:
  manual_and_event:
    manual:
      initiator_kinds: ["user"]
    event:
      kind: "vendor.message_received"
      filter: "input.timestamp.hour >= 22 || input.timestamp.hour < 7"
```

A workflow that may fire from either surface. Both sub-triggers are
individually validated.

## Inputs

Each input is a map entry whose name is the input identifier and whose
value is an input spec:

```yaml
inputs:
  depth:
    type: number
    required: false
    default: 2
    description: "Maximum recursion depth for nested lookups."
```

Input fields:

| Field | Type | Required | Notes |
|---|---|---|---|
| `type` | `"string" \| "number" \| "boolean" \| "array" \| "object"` | yes | Matches the JSON-Schema primitive types in `schemas/workflow-v1.schema.json`. |
| `required` | boolean | yes | When `true`, the input MUST be supplied at admission. |
| `default` | any | no | Used when `required: false` and the caller omits the input. |
| `description` | string | no | Human description, max 2048 chars. |

## Capabilities

The `capabilities` list is the workflow-level ceiling. Each entry is a
canonical capability string from the registry:

```
Capability::ReadFiles
Capability::WriteFiles
Capability::NetworkEgress
Capability::Subprocess
Capability::ReadSecret
Capability::ReadEnv
Capability::SendUnattendedMessage
Capability::PublishExternal
Capability::AlterRepoSecurity
Capability::DestructiveDelete
Capability::LongLived
Capability::AutoMerge
Capability::UnrestrictedTool
```

Each node may declare a subset that overrides the workflow ceiling
(but never widens it). The compiler computes the effective set as the
intersection of the workflow ceiling and every node's declared set.

## Nodes

Every node is an object with the following fields:

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes | Stable, unique within the workflow version. |
| `kind` | `NodeKind` | yes | One of the v1 node kinds listed below. |
| `label` | string | no | Human-readable label. |
| `attempt` | retry policy | yes | Bound retry policy. |
| `when` | expression string | no | Run this node only when the expression evaluates true. |
| `capabilities` | list of capability strings | no | Per-node capability subset override. |
| `<kind-specific payload>` | object | yes | One of the kind-specific payloads below. |

### Retry policy (`attempt`)

```yaml
attempt:
  max_attempts: 3
  backoff:
    kind: exponential
    base_ms: 500
    max_ms: 8000
    jitter: 0.1
  retry_on: [TransientNetwork, TransientProvider]
```

`retry_on` is a list of `FailureClass` enum variants from
`engine/crates/workflow/src/domain.rs`. Anything not in the list is
terminal on first occurrence.

### Kind-specific payloads

| Kind | Required payload |
|---|---|
| `Agent` | `payload:` (markdown spec) |
| `Recipe` | `payload:` (recipe reference, e.g. `recipe://brews/recipes/morning-briefing.md@1`) |
| `Transform` | `payload:` (transform spec) |
| `Condition` | `test:` (expression string) |
| `Map` | `over:` (collection expression) |
| `Loop` | `until:` (expression) and `max_iterations:` (integer) |
| `WaitTimer` | `for_seconds:` (integer) |
| `WaitEvent` | `wait_for:` (event kind string) |
| `Approval` | `require:` (principal/role spec) |
| `Effect` | `effects:` (list of effect declarations, draft-only) |
| `Subworkflow` | `subworkflow_ref:` (canonical workflow id) |

Each `kind` maps 1:1 to `NodeKind` in `domain.rs`. The compiler rejects
unknown kinds and unknown fields.

## Edges

```yaml
edges:
  - { from: start,            to: gather-overnight }
  - { from: gather-overnight,  to: summarize }
  - { from: summarize,         to: render-into-message }
  - { from: render-into-message, to: present-to-user }
  - { from: present-to-user,   to: end }
```

`from` may be a node id, the literal string `"start"`, or the literal
string `"end"`. Edges may include `when:` for conditional routing off
Condition nodes.

## Budgets

```yaml
budgets:
  cost_usd: 0.40
  tokens: 8000
  wall_time_seconds: 300
  depth: 2
```

Every budget is a hard ceiling. Exceeding any of them terminates the
run with `FailureClass::BudgetExceeded`.

## Expression syntax

Expressions are evaluated by `parse_expr` in
`engine/crates/workflow/src/expression.rs`. The v1 grammar supports:

- Logical operators: `&&`, `||`, `!`
- Comparison operators: `==`, `!=`, `<`, `<=`, `>`, `>=`
- JSON literals: numbers, strings (double-quoted), booleans, `null`, arrays, objects
- Path references of the form `<node-id>.output.<field>` (e.g.
  `summarize.output.summary`)
- Trigger payload access via `input.<field>` (e.g. `input.timestamp.hour`)
- Iteration access inside `Map` / `Loop` via `iteration.item` and `iteration.index`

The grammar is non-Turing-complete, depth-bounded, size-bounded, and
deterministic. Arbitrary code evaluation and implicit model evaluation
are forbidden.

## Compile output (canonical JSON shape)

When the compiler canonicalizes a fixture, it produces:

```json
{
  "schema_version": 1,
  "compiled_plan": {
    "entry_node": "gather-overnight",
    "nodes": [
      { "id": "gather-overnight", "kind": "Agent", "label": "...", "capabilities": ["Capability::ReadFiles","Capability::NetworkEgress"], "edges": [...], "retry": {...} },
      ...
    ],
    "effective_capabilities": ["Capability::ReadFiles","Capability::NetworkEgress"]
  },
  "content_hash": "sha256:<hex>",
  "version_id": "wfv_<11-hex>"
}
```

The dogfood test must assert:

- `compiled_plan.entry_node` equals the first declared node id.
- `compiled_plan.nodes.len()` equals the declared node count.
- `compiled_plan.effective_capabilities` is a lexicographically sorted,
  deduplicated list (see `CapabilitySet::from_iter`).
- `compiled_plan.nodes[*].id` values are unique.
- The terminal node has zero outgoing edges, or its sole outgoing edge
  targets `end`.

## Minimal complete example

```yaml
kind: coderight-workflow/v1
schema_version: 1
id: wfd-hello-world
name: Hello world
description: A trivial one-node workflow that produces a greeting.
trigger:
  manual:
    initiator_kinds: ["user"]
inputs:
  name:
    type: string
    required: true
    description: "Name to greet."
capabilities:
  - Capability::ReadFiles
nodes:
  - id: greet
    kind: Transform
    label: Build greeting
    attempt:
      max_attempts: 1
      backoff:
        kind: fixed
        base_ms: 0
      retry_on: []
    payload:
      transform: "format"
      template: "Hello, ${inputs.name}!"
edges:
  - { from: start, to: greet }
  - { from: greet, to: end }
budgets:
  cost_usd: 0.01
  tokens: 100
  wall_time_seconds: 30
  depth: 1
```

## Forbidden in v1

The v1 control plane explicitly blocks the following node kinds,
capabilities, and effect destinations. The compiler and the runtime
must reject every fixture that requests them.

### Forbidden node kinds

- `UntypedNode`, `Subprocess` (raw shell escape), `RawShell`,
  `SelfModify`, `PromptInjectionSink`.

### Forbidden capabilities

These capabilities MUST NOT appear in any workflow-level or per-node
`capabilities` list. They are reserved for v2+ and would silently widen
authority:

- `Capability::AutoMerge`
- `Capability::UnrestrictedTool`
- `Capability::AlterRepoSecurity`
- `Capability::DestructiveDelete`
- `Capability::PublishExternal` (anywhere except inside an `Effect`
  node with `effects: [{ kind: "draft", ... }]`; the runtime enforces
  draft-only at dispatch)
- `Capability::SendUnattendedMessage` (anywhere except inside an
  `Effect` node with draft-only `effects:` payload; the runtime
  enforces draft-only at dispatch)

If a fixture requests one of these capabilities outside a draft-only
Effect, the compiler emits `ForbiddenEffect` and refuses to publish.

### Forbidden effect destinations

`Effect` nodes may only target:

- `coderight://review-inbox`
- `coderight://desktop-draft`
- `coderight://workspace-draft`

Publishing directly to email, Slack, GitHub PRs, or any external system
is forbidden in v1. Reference workflows use the desktop-draft and
review-inbox destinations exclusively.

### Forbidden runtime behaviors

- No unbounded retries.
- No raw transcript inheritance between nodes.
- No automatic prompt/policy/skill/workflow self-modification.
- No "keep going" infinite mode.
- No execution outside the effective capability ceiling.

A fixture that depends on any of these is invalid for v1; the dogfood
test asserts compile-time rejection.
