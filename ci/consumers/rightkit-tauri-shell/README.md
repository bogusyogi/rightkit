# rightkit-tauri-shell 0.5.0 registry consumer (PK08)

Standalone consumer, outside the workspace, pinning `rightkit-tauri-shell =0.5.0` exactly with
default features. It exercises, through public API only:

- navigation scheme rules: `external_scheme_allowed`, `validate_internal_scheme`;
- caller/window ACL label rules (PK04/PK08): `caller_gate::is_content_label` exact and
  `prefix-*` matching, with app/trusted window labels never matching;
- WebView2 profile policy (PK08-1): `webview2_env::plan` refuses remote-debugging and
  no-sandbox arguments;
- preview store (PK08-5): `PreviewStore` register/lookup/revoke under a declared kind, with an
  undeclared kind refused.

State: in-memory only; OS entropy is read for the preview token. No network, no files, no
process spawn, no user data. Run only by the CI consumer lane. The crate compiles Tauri on the
native host, but this recipe starts no webview.

Not covered: webview creation, IPC, windows, menus, the updater, and any Tauri runtime. The
full caller decision (`caller_gate::decide`) needs a live webview URL, so only its pure label
rule runs here; host-service, licence, update, models, PDF and app-lock commands need their
feature rows in `ci/gate.config.json`, not this default-feature recipe.
