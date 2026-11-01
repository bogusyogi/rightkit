import { useEffect, useState } from 'react';
import { DesktopWorkspace } from './components/DesktopWorkspace';
import { AppStateProvider } from './lib/app-state';
import { resolveStartup, restartSetup, type Startup } from './lib/onboarding';
import { hydrateTheme } from './lib/theme';
import { Setup } from './views/Onboarding';

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
