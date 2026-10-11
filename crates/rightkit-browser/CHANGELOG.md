# Changelog

## Unreleased

- Launch passes `--disable-component-update`, `--disable-domain-reliability` and `--no-pings`. With `--disable-background-networking` alone Chrome still asked for `clients2.google.com` (the component updater), which reached a session's resolver and network policy as if a page had asked. A session now sees only its pages' requests.
- Windows: `BrowserSession::launch` checks that Chrome's sandbox can read the browser before starting it and returns the new `BrowserError::ChromeSandboxAccess { directory, principals, fix }` when it cannot. A Chrome unpacked from an archive outside `Program Files` has no read and execute grant for `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) and `ALL RESTRICTED APPLICATION PACKAGES` (`S-1-15-2-2`); Chrome then logs "Sandbox cannot access executable", its network service crashes at startup, and a first navigation hangs or aborts. This was the cause of the Windows navigation timeouts on CI. The error names the directory and the `icacls` command that grants the access. A launch with `no-sandbox` in `extra_args` is not checked, and an access list that cannot be read does not stop a launch. `BrowserError` gains a variant: an exhaustive `match` on it needs a new arm.

## 0.2.4

- Trim the dependency tree: tokio no longer enables `fs`, `macros` or `rt-multi-thread` for production builds (the e2e tests move them to dev-dependencies), and `chromiumoxide` is used with `default-features = false`. No public API change.

## 0.2.1

- Add `LaunchOptions::profile_dir(path)` & `ProfileSpec::Directory(PathBuf)` for
  existing absolute user-data directories. Preserve spaces, Unicode & native
  path bytes; validate directory existence/type without named-profile rules,
  creation or temporary-profile cleanup.
- Add `LaunchOptions::system_chrome_only()` for exclusive `find_chrome()`
  resolution with `ChromeNotFound` on absence. Keep default CfT selection &
  `system_chrome()` fallback behavior.
- Search RightKit-managed CfT versions before existing install/cache locations,
  newest numeric version first; support `RIGHTKIT_CHROME_FOR_TESTING_ROOT`.
  Explicit executable overrides retain priority.
- Add browser-free unit coverage for profile validation/preservation, native
  argv handling, Chrome selection & managed discovery. Defaults unchanged.
