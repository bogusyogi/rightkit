import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App.js';
import { ErrorBoundary, ShellProvider } from '@rightkit/react-shell/react';
import { installWindowCrashCapture } from '@rightkit/logs';
import { invoke } from '@tauri-apps/api/core';
import { startLicense } from './lib/license.js';
import { installFrontendDiagnostics, logs } from './lib/logs.js';
import { themeStore } from './lib/theme.js';
import { hostAppName } from "./appName.js";

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
