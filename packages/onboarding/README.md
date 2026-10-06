# @rightkit/onboarding

First-run onboarding framework for Right Suite apps: a legal agreement gate first, then ordered steps (permissions, model download, licence/trial, plus app-defined steps) with progress, skip and resume.

```tsx
import { Onboarding } from "@rightkit/onboarding";
import "@rightkit/onboarding/onboarding.css";

<Onboarding settings={settings} legal={legal} permissions={permissions} models={models} license={license}
  onComplete={finish} onSkip={later} />
```

- `settings` is a `SettingsBackend`; progress is stored under the `onboarding.state` key.
- The agreement is enforced whenever `legalAccepted` is not persisted as `true`, regardless of the saved step index.
- `onComplete` and `onSkip` run only after the state was saved. A rejected save is shown in an alert and neither callback runs.
