# Right Suite Licensing — Architecture (v2: signed entitlements, Rust enforcement)

**Status:** design, 2026-06-21. Supersedes the enforcement model of v1 (server-authoritative
validation + JS soft-gate). v1's *validation* stays; v1's *enforcement* is the gap this closes.

> One-line: **server signs entitlements, Rust verifies + enforces them, JS carries them + drives the UI.**
> Three layers, one backend, two client libraries.

---

## 1. Why v1 isn't enough

v1 made **validation** server-authoritative (you can't forge a license; revocation lands on
refresh). But **enforcement** — the actual "stop a non-Pro user from doing the thing" — sits in
JavaScript. Audit of HeardRight, 2026-06-21:

- Pro gate is `isPro(license)` in `Shortcuts.tsx:23` (JS). `LicenseSettings.tsx` only displays state.
- The Rust side does **nothing** but store the blob in the keyring (`secure_license_get/set/delete`).
- The real features run in the `heardright-engine` sidecar — but JS decides whether to invoke them.

So: open DevTools, force `isPro` true → wake word / long transcription / shortcuts unlock. The
server validated a license beautifully; the gate guarding the feature is a JS boolean anyone can flip.
**Validation ≠ enforcement.** CodeRight, MailRight, and ViewRight have the same JS-gate shape today.
ScrapeRight is the only app that already enforces in Rust (offline Ed25519) — its instinct was right.

## 2. Threat model (what we defend, what we concede)

| Attack | Defended? | By what |
|---|---|---|
| Forge a license / fake "active" | ✅ | Server is the only issuer; tokens are signed |
| Edit JS to flip `isPro` → use a local Pro feature | ✅ (v2) | Gate moves to Rust; JS can't make Rust act |
| Fabricate a stored blob, feed it to the keyring | ✅ (v2) | Rust verifies the **signature**, not just the JSON |
| Keep Pro after a refund/chargeback | ✅ | Short token `exp` + refresh stops re-signing |
| Copy the token to another machine | ◑ | Token is `deviceId`-bound; needs device-id spoofing too |
| Replay an old valid token | ◑ | Bounded by `exp` (revocation speed ↔ offline grace tradeoff) |
| **Patch the signed binary** (NOP the Rust verify) | ❌ | The hard ceiling — defeats code-signing, stops all but the determined few |

**Spectrum, by where a feature runs:**
`JS gate (trivial)` → `Rust + signature gate (must patch a signed binary)` → `server-only (unbypassable)`.
A *server-side* feature (a Pro cloud API, the update binary) can be made unbypassable — the server
checks the token on each request. An *on-device* feature (transcription, wake word, BYOK AI) tops out
at the Rust gate; the server isn't in the loop at use-time. That ceiling is inherent, not a shortcoming —
the goal is to climb from the JS floor to the Rust ceiling.

## 3. The three layers

```
        ┌──────────────────────────── rightapps-api (ONE backend) ───────────────────────────┐
        │  AUTHORITY + SIGNER:  validates the license, mints a SHORT-LIVED token signed with   │
        │  the backend PRIVATE key →  { v, app, deviceId, licenseId, tier, features[], iat,exp }│
        └───────────────▲───────────────────────────────────────────────────────────┬─────────┘
        activate/refresh │ (POST, via Tauri HTTP plugin — bypasses CORS)              │ signed token
                         │                                                            ▼
   ┌─────────────────────┴───────────────┐                       ┌──────────────────────────────────┐
   │  JS  ·  @rightkit/license            │   stores token via    │  Rust ·  rightkit-license (crate) │
   │  NETWORK + UX                        │──────────────────────▶│  VERIFY + ENFORCE                  │
   │  activate/refresh/store; Pro badge,  │   keyring command     │  embeds backend PUBLIC key;        │
   │  activation form. NOT the gate.      │                       │  every Pro command: verify sig +   │
   │  (edit it → you fake only the UI)    │                       │  exp + deviceId → tier → allow/deny │
   └──────────────────────────────────────┘                       └──────────────────────────────────┘
```

- **Server (authority + signer).** Already builds entitlements (`buildEntitlements`); now it also
  **signs** them. Private key lives server-side only (env/secret store), never in a repo. Revocation =
  refuse to re-sign (status `revoked` → refresh returns `invalid`, no token).
- **JS (`@rightkit/license`).** Unchanged role: activate / refresh / deactivate / store + drive UI.
  Gains one field: it carries the opaque `signedToken` through to storage. It does **not** verify or
  gate — editing it only changes what the UI *says*, never what the app *does*.
- **Rust (`rightkit-license`).** The enforcement primitive. Promote ScrapeRight's existing crate
  (it already has `LICENSE_PUBKEY` + Ed25519 verify) to a **shared** crate every app depends on.
  Holds the public key; exposes `verify(token) -> Entitlement` and `tier_at_least()/has_feature()`.
  Every Pro-gated Rust command calls it before doing the work.

## 4. The token

- **Format:** compact Ed25519-signed token. Recommend a JWT with `EdDSA` (interop, libraries) or a
  custom `base64(payload).base64(sig)` if we want zero JWT deps (ScrapeRight's crate is already custom).
  Decide in Phase 1; the claim set is the same either way.
- **Claims:** `{ v:1, app, deviceId, licenseId, tier, features[], iat, exp }`. `v` versions the claim
  schema; `exp` is the teeth.
- **Lifetime + refresh:** token `exp` is **short** (proposed 14–30 days). The *license* can be perpetual;
  the *token* is always short-lived and re-minted on refresh. A lifetime buyer is never bricked — refresh
  re-signs as long as the license is active; offline, the per-kind grace ([isLicensed]) covers the gap.
  Shorter `exp` = faster revocation, less offline tolerance. **This is the one real tuning knob.**
- **Binding:** `deviceId` claim; Rust checks it matches this device. Seat *limits* stay server-side.
- **Revocation:** server stops signing → the token dies at `exp`. No CRL needed at our scale.

## 5. Key management

- **Private key:** server-only (rightapps-api secret). Treat like the Apple `.p8` / updater keys — never
  in any repo, never in a client.
- **Public key:** embedded in each app's Rust binary (as ScrapeRight does today).
- **Rotation:** put a `kid` (key id) in the token; ship apps that trust a *set* of public keys; rotate
  with an overlap window (new key signs, old key still verified until all clients update).

## 6. Component changes

### 6a. Server (`rightapps-api`) — do once, all apps benefit
- Add a signer: on `activate` + `refresh`, sign the entitlement and return `signedToken` alongside the
  existing JSON. Additive — old clients ignore the new field.
- Hold the private key in env; expose the public key(s) for embedding.
- Revocation path unchanged (revoked → refresh returns `invalid`, omit token).

### 6b. JS (`@rightkit/license`) — do once
- `StoredLicense` gains `signedToken?: string`. `activate`/`refresh` capture + persist it. No verification
  in JS. Bump SDK; additive + back-compatible.

### 6c. Rust (`rightkit-license` shared crate) — do once, then each app wires it
- Promote ScrapeRight's crate to a shared crate (git dep or workspace). API sketch:
  `verify(token: &str) -> Result<Entitlement>`, `Entitlement { tier, features, device_id, exp }`,
  `entitlement.tier_at_least(Tier::Pro)`, `entitlement.has_feature("wake_word")`.
- Each app: store the token in its keyring; on each Pro command, `load → verify → gate`.

## 7. Per-app gate map (where enforcement must move)

| App | Pro features | Run where | Today | v2 work |
|---|---|---|---|---|
| **HeardRight** | wake word, voice commands, >N-min transcription, BYOK AI | `heardright-engine` sidecar via Rust cmds | **JS soft-gate** (`Shortcuts.tsx`) | gate at the Rust commands that drive the engine; add verify |
| **ScrapeRight** | Plus/Pro ladder, BYOK AI, update channel | Rust | **already Rust-enforced (offline Ed25519)** | swap verifier input: offline blob → server-signed token; keep gates |
| **CodeRight** | routing, brews, evolution, execution backends, a2a, schedules | daemon | JS (`updateLicense.features`) | gate at the daemon/Rust boundary |
| **MailRight** | (updates only, currently) | — | update endpoint = server-gated | low urgency; gate in Rust only if a local Pro feature is added |
| **ViewRight** | (updates only) | — | update endpoint = server-gated | same as MailRight |

Enforcement work concentrates on **HR, ScrapeRight, CodeRight** (apps with *local* Pro features). MR/VR
gate updates via the updater endpoint, which is already server-side — fine until they ship a local Pro feature.

## 8. Rollout — who implements what, when

Additive and back-compatible throughout: tokens are added alongside the existing fields; each app
*starts enforcing* only when its Rust lands. Nothing breaks between phases.

- **Phase 1 — shared groundwork (done centrally, not per app):**
  (a) server signs tokens on activate/refresh; (b) `@rightkit/license` carries `signedToken`;
  (c) promote ScrapeRight's Rust crate to the shared `rightkit-license` crate. After this, tokens flow +
  are storable everywhere, but no app enforces yet.
- **Phase 2 — per-app enforcement (each app implements):** embed the public key, depend on the shared
  crate, and move each Pro gate from JS to the Rust command boundary. Order by exposure: **HeardRight →
  ScrapeRight → CodeRight**. MailRight/ViewRight only when they add a local Pro feature.
- **Phase 3 — ScrapeRight cutover:** retire its offline mint + signing key; point its (now shared) Rust
  verifier at server-issued tokens.

**So: the shared pieces (server + 2 libs) are built once; then yes — each app implements its own Rust
gates.** The JS migration already done is the network/UX layer and stays; v2 adds the Rust enforcement
layer beneath it.

## 9. Open decisions
- Token `exp` length (revocation speed ↔ offline grace).
- JWT-EdDSA vs ScrapeRight's existing custom Ed25519 format (reuse vs interop).
- Per app: exact list of Rust command boundaries to gate (needs a short per-app audit like §7's HR one).
- Whether to also verify in the `heardright-engine` sidecar itself (belt-and-suspenders) or only at the
  Tauri Rust command layer that fronts it.

## 10. Unchanged from v1
One unified backend; `@rightkit/license` as the JS network/UX client; server-authoritative validation;
per-kind offline grace (`isLicensed`); the creator license. v2 only adds the signing + Rust-verify +
Rust-gate layer beneath the existing client.
