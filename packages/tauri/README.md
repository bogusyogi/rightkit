# `@rightkit/tauri`

Typed renderer-side adapters for the shared Right Suite Tauri command surface.

The client accepts the app's real `invoke` function and an explicit command
map. App-specific command names and Rust argument names stay in a thin local
adapter; license, device, structured-log, and diagnostics calls use one typed
client implementation.

The device adapter exposes one `getOrCreate` operation. Standard hosts use the
shared secure get/set pair; legacy hosts may map one atomic native command.
Updater behavior is composed from the existing `@rightkit/updates` client rather
than reimplemented. Structured-log bundles and app diagnostics have distinct
generic result types because they are not interchangeable on every host.

This package remains workspace-internal during Phase 3. Publish it only after
all five core apps pass the same command and updater contract suite. Apps must
then consume an exact npm version—never a path, Git, `file:`, `link:`, or
`workspace:` dependency.
