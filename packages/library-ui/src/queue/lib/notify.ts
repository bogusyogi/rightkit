import { getShell } from "./ipc";
import { hostAppName } from "../../appName";

/**
 * Ready notification through the RightKit shell. The shell requests OS permission on post, and it
 * posts natively on macOS and Windows. Notifications are best-effort: a refused post must not
 * reject the job-state handler that calls this.
 */
export async function notifyReady(body: string): Promise<void> {
  try {
    await getShell().notifications.post({ title: hostAppName(), body });
  } catch {
    // Unsupported host or OS refusal; the job is still ready in the UI.
  }
}
