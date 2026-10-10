// appdmg-style DMG spec (`dmg build --spec <file.json>`, `targets.mac.dmg.spec`) mapped onto the
// headless hdiutil pipeline in dmg-build.mjs. Only fields the pipeline can render are accepted; any
// other appdmg field fails with DMG_SPEC_UNSUPPORTED instead of being silently dropped.
//
// Supported:  title, icon (.icns volume icon), background (PNG/TIFF), icon-size,
//             window.size {width,height}, window.position {x,y}, format "UDZO", filesystem "HFS+",
//             contents: one `file` entry for the .app and one `link` entry to /Applications (x,y = icon centre).
// Unsupported: background-color, code-sign (the release pipeline signs the DMG), license, other formats and
//             filesystems, extra files or links, `position` entries, any unknown key.
// Relative `background` / `icon` paths resolve against the spec file's directory.
import { readFileSync } from "node:fs";
import path from "node:path";

const fail = (message, code = "DMG_SPEC_INVALID") => Object.assign(new Error(message), { code });
const unsupported = (message) => fail(message, "DMG_SPEC_UNSUPPORTED");
const SPEC_KEYS = new Set(["title", "icon", "background", "icon-size", "window", "format", "filesystem", "contents"]);
const UNSUPPORTED_KEYS = new Map([
  ["background-color", "background-color is not rendered; use a background image"],
  ["code-sign", "code-sign is not supported: right-release Developer ID signs the DMG in its own sign step"],
  ["license", "license agreements are not supported"],
]);
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const int = (value, label, { min = 0 } = {}) => {
  if (!Number.isInteger(value) || value < min || value > 32767) throw fail(`${label} must be an integer ${min}..32767`);
  return value;
};

/** Parse an appdmg-style spec object into buildDmg options. `app` (optional) is the .app path being packed. */
export function parseDmgSpec(spec, { baseDir = process.cwd(), app } = {}) {
  if (!isObject(spec)) throw fail("DMG spec must be a JSON object");
  for (const key of Object.keys(spec)) {
    if (UNSUPPORTED_KEYS.has(key)) throw unsupported(`DMG spec: ${UNSUPPORTED_KEYS.get(key)}`);
    if (!SPEC_KEYS.has(key)) throw unsupported(`DMG spec: unsupported field ${JSON.stringify(key)}`);
  }
  const options = {};
  if (spec.title !== undefined) {
    if (typeof spec.title !== "string" || !spec.title) throw fail("DMG spec: title must be a non-empty string");
    options.volname = spec.title;
  }
  const file = (value, label) => {
    if (typeof value !== "string" || !value) throw fail(`DMG spec: ${label} must be a non-empty path string`);
    return path.resolve(baseDir, value);
  };
  if (spec.icon !== undefined) options.volumeIcon = file(spec.icon, "icon");
  if (spec.background !== undefined) options.background = file(spec.background, "background");
  if (spec["icon-size"] !== undefined) options.iconSize = int(spec["icon-size"], "DMG spec: icon-size", { min: 16 });
  if (spec.format !== undefined && spec.format !== "UDZO") throw unsupported(`DMG spec: format ${JSON.stringify(spec.format)} is not supported (the pipeline writes UDZO)`);
  if (spec.filesystem !== undefined && spec.filesystem !== "HFS+") throw unsupported(`DMG spec: filesystem ${JSON.stringify(spec.filesystem)} is not supported (the pipeline writes HFS+)`);
  if (spec.window !== undefined) {
    if (!isObject(spec.window)) throw fail("DMG spec: window must be an object");
    for (const key of Object.keys(spec.window)) if (!["size", "position"].includes(key)) throw unsupported(`DMG spec: unsupported field window.${key}`);
    if (spec.window.size !== undefined) {
      const { width, height, ...extra } = isObject(spec.window.size) ? spec.window.size : { width: undefined };
      if (Object.keys(extra).length) throw unsupported(`DMG spec: unsupported field window.size.${Object.keys(extra)[0]}`);
      options.window = [int(width, "DMG spec: window.size.width", { min: 1 }), int(height, "DMG spec: window.size.height", { min: 1 })];
    }
    if (spec.window.position !== undefined) {
      const { x, y, ...extra } = isObject(spec.window.position) ? spec.window.position : { x: undefined };
      if (Object.keys(extra).length) throw unsupported(`DMG spec: unsupported field window.position.${Object.keys(extra)[0]}`);
      options.windowPosition = [int(x, "DMG spec: window.position.x"), int(y, "DMG spec: window.position.y")];
    }
  }
  if (spec.contents !== undefined) {
    if (!Array.isArray(spec.contents)) throw fail("DMG spec: contents must be an array");
    const appName = app ? path.basename(app) : null;
    spec.contents.forEach((item, index) => {
      const label = `DMG spec: contents[${index}]`;
      if (!isObject(item)) throw fail(`${label} must be an object`);
      for (const key of Object.keys(item)) if (!["x", "y", "type", "path", "name"].includes(key)) throw unsupported(`${label}: unsupported field ${JSON.stringify(key)}`);
      const point = [int(item.x, `${label}.x`), int(item.y, `${label}.y`)];
      if (typeof item.path !== "string" || !item.path) throw fail(`${label}.path must be a non-empty string`);
      if (item.type === "link") {
        if (item.path !== "/Applications") throw unsupported(`${label}: only a link to /Applications is supported (got ${item.path})`);
        if (options.applicationsPosition) throw fail(`${label}: duplicate /Applications link`);
        options.applicationsPosition = point;
      } else if (item.type === "file" || item.type === undefined) {
        const name = item.name ?? path.basename(item.path);
        if (!name.endsWith(".app")) throw unsupported(`${label}: only the .app can be a file entry (got ${item.path}); extra files are not supported`);
        if (appName && name !== appName) throw fail(`${label}: names ${name} but the app being packed is ${appName}`);
        if (options.appPosition) throw fail(`${label}: duplicate .app entry`);
        options.appPosition = point;
      } else if (item.type === "position") throw unsupported(`${label}: position entries are not supported`);
      else throw fail(`${label}.type must be "file" or "link" (got ${JSON.stringify(item.type)})`);
    });
  }
  return options;
}

/** Read and parse a spec file. */
export function readDmgSpec(specPath, { app } = {}) {
  const file = path.resolve(specPath);
  let text;
  try { text = readFileSync(file, "utf8"); } catch (error) { throw fail(`DMG spec not readable: ${file} (${error.code ?? error.message})`); }
  let spec;
  try { spec = JSON.parse(text); } catch { throw fail(`DMG spec is not valid JSON: ${file}`); }
  return parseDmgSpec(spec, { baseDir: path.dirname(file), app });
}
