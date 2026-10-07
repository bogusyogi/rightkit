# Changelog

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
