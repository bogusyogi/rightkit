import { sectionClient } from "../settings/client.js";

/** The `speech` client group (local model status, download, engine warm-up) the setup uses. */
export function speechClient() {
  return sectionClient("speech");
}
