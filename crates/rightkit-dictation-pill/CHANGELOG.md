# Changelog

## 0.1.2

- Add platform-free `session_glue`: `schedule_reveal_after_splash` (+ `RevealAfterSplash`, `LogSink`) over `RevealRetry`, and `apply_guardian_batch` (+ `FullscreenSampling`) for one guardian pass. The host passes the splash probe, main-thread dispatcher, reveal closures, log sink and event names. Additive.
- Add `clip_override::dismissal_releases_holds(generation)`: the host's hold-release rule after a card dismisses itself. Additive.

## 0.1.1 candidate

- Add caller-required geometry, motion, hint/cadence, event pipeline & session infrastructure with host-supplied presentation inputs.
- Add host-configured native C bridge & generation-aware clipboard override lifecycle, including settled refresh identity.
- Carry splash generations through truthful show/dismiss acknowledgment, forced completion & close callback; buffer reentrant close & sample app readiness outside Rust mutex.
- Suppress duplicate initialize/shutdown & post-shutdown entry admission.

## 0.1.0 (unreleased)

- Add native bridge C-string bounds, callback panic guard & host-configured splash handoff/reveal retry.
- Add guardian start/stop slot, Windows restart supervision & host-title no-activate sweep; retain existing guardian panic containment & bounded drains.
- Add Windows pill, hint & clipboard overlay controllers with host-supplied painters, labels, geometry, motion & policy.
- Add generation-scoped startup, cancellation, retained-thread retirement & bounded command queues.
- Add completion tokens & deadlines for clipboard & host actions.
- Add platform-neutral event queues, hitbox storage, bridge validation & guardian coalescing, with native Windows & macOS event sources.
- Bound guardian drains to one queue capacity before servicing due work & rechecking stop.
- Contain host readiness panics & report callback-specific payload types; clear guardian generation slots & signal exit after unexpected worker panics.
- Import objc2 `DefinedClass` for macOS observer ivar access.
- Include MIT & Apache-2.0 licenses; pin version-compatible RightKit dependencies.
