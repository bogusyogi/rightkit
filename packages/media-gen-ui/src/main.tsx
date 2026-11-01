import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { ErrorBoundary, ShellProvider } from '@rightkit/react-shell/react';
import { installWindowCrashCapture } from '@rightkit/logs';
import { invoke } from '@tauri-apps/api/core';
import { startLicense } from './lib/license';
import { installFrontendDiagnostics, logs } from './lib/logs';
import { themeStore } from './lib/theme';
import './styles.css';
import '@rightkit/media-ui/styles.css';
import { hostAppName } from "./appName";

installFrontendDiagnostics();
// Renderer errors and unhandled rejections go to the native crash recorder (scrubbed, rate-limited, local only).
installWindowCrashCapture({ invoke });
void startLicense();

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <ShellProvider themeStore={themeStore}>
      <ErrorBoundary
        label={hostAppName()}
        onError={(error) => {
          logs.error('render_error', { surface: 'error_boundary', component: error.name });
          void logs.flush();
        }}
      >
        <App />
      </ErrorBoundary>
    </ShellProvider>
  </React.StrictMode>
);
