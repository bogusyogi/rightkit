import { useEffect, useState } from 'react';
import { DesktopWorkspace } from './components/DesktopWorkspace.js';
import { AppStateProvider } from './lib/app-state.js';
import { resolveStartup, restartSetup, type Startup } from './lib/onboarding.js';
import { hydrateTheme } from './lib/theme.js';
import { Setup } from './views/Onboarding.js';

export default function App() {
  const [mode, setMode] = useState<Startup | null>(null);
  useEffect(() => {
    void hydrateTheme().then(resolveStartup).then(setMode);
  }, []);
  const reopenSetup = () => void restartSetup().finally(() => setMode('setup'));
  return (
    <AppStateProvider>
      {mode === null ? null : mode === 'app' ? <DesktopWorkspace onOpenSetup={reopenSetup} /> : <Setup onDone={() => setMode('app')} />}
    </AppStateProvider>
  );
}
