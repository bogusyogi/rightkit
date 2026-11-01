import { fileURLToPath } from "node:url";

/** Absolute path of the shipped worker source (render_job.py, renderright/, tools/, tests/). */
export const workerDir = fileURLToPath(new URL("./worker/", import.meta.url));
