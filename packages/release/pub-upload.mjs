#!/usr/bin/env node
import { runLargeUpload } from "./upload-large.mjs";
try { runLargeUpload(process.argv.slice(2)); }
catch (error) { console.error(`pub-upload: ${error.message}`); process.exitCode = 1; }
