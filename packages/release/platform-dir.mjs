// The ONE spelling of a platform's directory under `.right-release/sealed/<release id>/` and `.right-release/state/<release id>/`. The CLI and the
// gate say `win`; build-release seals into `windows`. A producer or reader that joined the CLI value (gate-stages.mjs sealedFor, the draft packer)
// looked in a directory nothing wrote (audit GB-Windows-platform-dir); every writer and reader of those trees resolves through this.
// A LEAF module: node builtins only.
export const PLATFORM_DIRS = Object.freeze({ win: "windows", windows: "windows", mac: "mac", android: "android", ios: "ios" });

/** `win`|`windows` -> `windows`; `mac`, `android`, `ios` as is. Another well-formed name passes through; anything that could leave the directory is refused. */
export function platformDirOf(platform) {
  if (typeof platform !== "string" || !/^[a-z][a-z0-9-]{0,31}$/.test(platform)) throw new TypeError(`${JSON.stringify(platform ?? null)} is not a platform name`);
  return Object.hasOwn(PLATFORM_DIRS, platform) ? PLATFORM_DIRS[platform] : platform;
}

/** The CLI spelling of a platform directory (`windows` -> `win`), for a reader that walks the tree. */
export const platformOfDir = (dir) => (dir === "windows" ? "win" : dir);
