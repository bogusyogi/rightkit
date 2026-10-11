// Copies the stylesheet next to the package root and into dist. A Node script, not `cp` and
// `mkdir -p`, so the build runs the same in cmd.exe, PowerShell and POSIX shells.
import { copyFileSync, mkdirSync } from "node:fs";

mkdirSync("dist", { recursive: true });
copyFileSync("src/styles.css", "styles.css");
copyFileSync("src/styles.css", "dist/styles.css");
