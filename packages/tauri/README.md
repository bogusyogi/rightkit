# `@rightkit/tauri`

Typed renderer-side adapters for the shared Right Suite Tauri command surface.

The client accepts the app's real `invoke` function and an explicit command
map. It also exports canonical typed constructors for ViewRight, ScrapeRight,
HeardRight, MailRight, and CodeRight, so app-specific command and Rust argument
names live in one tested package without importing app business logic.

The device adapter exposes a no-argument `getOrCreate()` operation and invokes
only an atomic native command. The native host generates, stores, and returns
the identifier; the renderer never supplies identity material. A host exposing
separate get/set commands or a fallback-taking transitional command is marked
unsupported. All five canonical app configurations use
`secure_device_id_get_or_create`; the exported
`standardRightKitDeviceGetOrCreate` constant is the single shared command
contract.
Updater behavior is composed from the existing `@rightkit/updates` client rather
than reimplemented. Structured-log bundles and app diagnostics have distinct
generic result types because they are not interchangeable on every host.

This package remains workspace-internal during Phase 3. Publish it only after
all five core apps pass the same command and updater contract suite. Apps must
then consume an exact npm version—never a path, Git, `file:`, `link:`, or
`workspace:` dependency.

Licensed under either MIT or Apache-2.0, at your option.
