// A file the user picks to give an agent. The bytes go to the app, which stores
// them under a neutral name; no path is read or sent. Same limit as one draft.

/** Largest file one draft can carry (the app enforces this too). */
export const AGENT_FILE_MAX = 18 * 1024 * 1024;

/** Base64 of the file, built in chunks so a large file does not overflow the call stack. */
export async function fileToBase64(file: Blob): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const CHUNK = 0x8000;
  let bin = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(bin);
}
