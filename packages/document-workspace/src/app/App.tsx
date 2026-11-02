import { AppProvider } from "./AppProvider.js";
import { AppShell } from "./AppShell.js";

export function App() {
  return (
    <AppProvider>
      <AppShell />
    </AppProvider>
  );
}
