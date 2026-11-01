import { AppProvider } from "./AppProvider";
import { AppShell } from "./AppShell";

export function App() {
  return (
    <AppProvider>
      <AppShell />
    </AppProvider>
  );
}
