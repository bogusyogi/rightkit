// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Dev and test only. Reached solely through a dynamic import guarded by import.meta.env.DEV || __RIGHTKIT_MOCK__ in
// ../index.ts, so a release bundle never carries the mock or its fixtures. The shared `@rightkit/react-shell/data`
// source selection and fault injector are unpublished, so the pick below is local again.
import type { DataSource } from "../source";
import { MockDataSource } from "./mockDataSource";

/**
 * The engine only when asked for and reachable (Tauri plus either VITE_RIGHTKIT_DATA=engine, which plain `pnpm dev`
 * and `tauri dev` still use, or the init value the UI QA shell sets from its world, `RIGHTKIT_QA_APP_DATA_SOURCE=engine`);
 * otherwise the mock.
 */
export function pickDevSource(hostAvailable: boolean, engine: () => DataSource): DataSource {
  const asked = import.meta.env.VITE_RIGHTKIT_DATA === "engine" || window.__RIGHTKIT_QA_DATA_SOURCE__ === "engine";
  return hostAvailable && asked ? engine() : new MockDataSource();
}
