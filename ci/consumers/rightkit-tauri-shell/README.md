# rightkit-tauri-shell 0.8.6 registry consumer (PK08)

Standalone consumer, outside the workspace, pinning `rightkit-tauri-shell =0.8.6` exactly with
default features, `background-update` and `menubar-helper`. It exercises, through public API only:

- background update status starts at `Idle` in a fresh process;
- the typed microphone pane maps to the native Windows and macOS settings URIs;
- navigation scheme rules: `external_scheme_allowed`, `validate_internal_scheme`;
- caller/window ACL label rules (PK04/PK08): `caller_gate::is_content_label` exact and
  `prefix-*` matching, with app/trusted window labels never matching;
- WebView2 profile policy (PK08-1): `webview2_env::plan` refuses remote-debugging and
  no-sandbox arguments;
- preview store (PK08-5): `PreviewStore` register/lookup/revoke under a declared kind, with an
  undeclared kind refused.
- launch-origin (A4): pure login-origin decisions with explicit marker and uptime inputs.
- relaunch (A1): compile-checks public dispatch/spawn/relaunch signatures only; no helper is
  started and no live relaunch occurs.

State: in-memory only; OS entropy is read for the preview token. No network, no files, no
process spawn, no user data. Run only by the CI consumer lane. The crate compiles Tauri on the
native host, but this recipe starts no webview.

Not covered: webview creation, IPC, windows, menus, the updater, and any Tauri runtime. The
full caller decision (`caller_gate::decide`) needs a live webview URL, so only its pure label
rule runs here; host-service, licence, update, models, PDF and app-lock commands need their
feature rows in `ci/gate.config.json`, not this default-feature recipe.
