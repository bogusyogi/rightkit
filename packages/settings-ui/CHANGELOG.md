# Changelog

## 0.2.0

### Fixed
- Updates: "Install updates automatically" is shown only when `UpdatesOptions.supportsAutoInstall` is true (default false). It was always shown, even when the update client never auto-installs. Apps whose client sets `shouldAutoInstall` must now pass `supportsAutoInstall: true`.

### Added
- `SettingsSection.badge` (nav pill) and `SettingsSection.heading` (page heading distinct from the nav label); `SectionFrame` export.
- Nav icons for all built-in sections (`sectionIcons`), including Appearance and Legal.
- `LicenseOptions.planSlot`: plan/upsell panel above the licence rows.
- `AboutOptions.panels`: full panels rendered outside the About card (`extra` stays in-card).
- `UpdatesOptions.labels`: override Updates strings.
- `standardSections`: `about` is optional; `include`, `omit` and `override` select/adjust built-ins by id.
- Unit tests (`src/seams.test.tsx`).

### Changed
- Nav label is wrapped in `.rk-settings__label`; CSS tokens use the `--rk-*` prefix.
