import { pickFile as rkPickFile, pickFiles as rkPickFiles, pickFolder as rkPickFolder } from "@rightkit/tauri/dialog";

/**
 * The rightkit-qa suite answers native dialogs through the in-app control
 * server (served only when the QA launcher enables it): it sets this value and
 * the next dialog returns it instead of opening an OS sheet. Release builds compile it out.
 * The pickers are @rightkit/tauri/dialog; it has no QA answer source (its README: an app's QA answers
 * pickers in its own seam), so this seam stays until RightKit ships one (see the exception ledger).
 */
function qaAnswer(): string | string[] | null | undefined {
  if (!(typeof __RIGHTKIT_QA__ === "boolean" && __RIGHTKIT_QA__)) return undefined;
  return (window as unknown as { __RIGHTKIT_QA_DIALOG__?: string | string[] | null }).__RIGHTKIT_QA_DIALOG__;
}

export async function pickFolder(title: string): Promise<string | null> {
  const qa = qaAnswer();
  if (qa !== undefined) return typeof qa === "string" ? qa : null;
  return rkPickFolder({ title });
}

export async function pickMedia(): Promise<string[]> {
  const qa = qaAnswer();
  if (qa !== undefined) return qa === null ? [] : Array.isArray(qa) ? qa : [qa];
  const picked = await rkPickFiles({
    title: "Add media",
    filters: [
      {
        name: "Media",
        extensions: ["mp4", "mov", "mkv", "webm", "m4v", "png", "jpg", "jpeg", "webp", "wav", "mp3", "m4a", "flac"],
      },
    ],
  });
  return picked ?? [];
}

/** Pick one file (e.g. a retention CSV export). `extensions: null` = no filter. */
export async function pickFile(title: string, extensions: string[] | null): Promise<string | null> {
  const qa = qaAnswer();
  if (qa !== undefined) return typeof qa === "string" ? qa : null;
  return rkPickFile({
    title,
    filters: extensions && extensions.length > 0 ? [{ name: "Files", extensions }] : undefined,
  });
}
