# @rightkit/dictation-ui npm consumer

Packed-archive consumer for `@rightkit/dictation-ui` 0.3.0. It verifies exact export keys (`.`, `./onboarding`, `./package.json`, `./pill`, `./settings`), resolves each runtime entry, and checks representative headless dictation, pill, and onboarding exports. It also verifies delivery filtering preserves the host-facing process-name key.

The sealed archive is installed with the reviewed closure in `ci/consumer-recipes.json`. Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION`, and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). Output is one JSON line; `nativeHostQualified` is always false.
