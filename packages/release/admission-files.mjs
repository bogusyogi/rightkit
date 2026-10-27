// The files tier of the admission (admission.mjs): refusals decided by the release config and the committed files alone.
// Each row is the predicate of a refusal the build used to raise later, after the lock, the install or the compile; the
// message is the build's own, so the doctor, the build and the worker say the same thing. A row names where its twin lives.
import path from "node:path";
import { assertGitHubRepository, isSuiteUpdaterApp } from "./github-updater-release.mjs";
import { validateAndroidTarget } from "./android-release.mjs";
import { assertNsisInPlaceUpgradeContract } from "./nsis-upgrade-contract.mjs";
import { dirtyBuildInputs } from "./release-invocation.mjs";
import { packageCommandOptional } from "./project-type.mjs";
import { planMediaSidecar, validateMediaSidecarConfig } from "./media-sidecar.mjs";
import { loadSidecarCatalog } from "./media-sidecar-catalog.mjs";

export const WINDOWS_NSIS_SIGNING_CONTRACT = "windows-raw-exe-authenticode-before-nsis-v1";
export const DISTRIBUTION_PROVIDERS = Object.freeze(["r2-rightapps", "github-releases"]);
const SAFE_APP = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const SAFE_VERSION = /^[A-Za-z0-9][A-Za-z0-9._+-]*$/;
// publish-update.mjs assertCurrentKey / assertVersionedKey: where the R2 objects of a private release live.
const CURRENT_UPDATER_KEY = /\/updates\/(?:mac|windows)\/current\//;
const CURRENT_INSTALLER_KEY = /\/installers\/(?:mac|windows)\/current\//;
const VERSIONED_INSTALLER_KEY = /\/installers\/(?:mac|windows)\/versions\/[^/]+\/[^/]+$/;

const isNsis = (ctx) => ctx.platform === "win" && ctx.target?.signingContract === WINDOWS_NSIS_SIGNING_CONTRACT;
const asList = (value) => (Array.isArray(value) ? value : []);
const app = (ctx) => ctx.config?.app ?? "app";

// build-release.mjs (the controller) and release.mjs (the worker) read the same "which file is the app's package.json" answer.
const workdirOf = (ctx) => path.resolve(ctx.root, ctx.config?.workdir ?? ".");

export const FILES_ROWS = Object.freeze([
  {
    // build-release.mjs: the config must expose a release identity that is safe to put in paths and ids.
    id: "config-identity",
    tier: "files",
    needsTarget: false,
    run({ config }) {
      const found = [];
      if (config?.schema !== 1) found.push(`unsupported config schema: ${config?.schema ?? "<missing>"} (expected 1)`);
      if (!config?.app || !config?.version) found.push("release config must expose app and version");
      else if (!SAFE_APP.test(config.app) || !SAFE_VERSION.test(config.version)) found.push("app and version must be safe release-id components");
      return found;
    },
  },
  {
    // release.mjs `has no <platform> package command`; build-release.mjs said the same after the lock.
    id: "package-command",
    tier: "files",
    run(ctx) {
      if (ctx.platform === "android" || ctx.platform === "ios") return { na: "android and ios build without a package command" };
      if (ctx.target.package || packageCommandOptional(ctx.target, ctx.projectType(), { expanded: true })) return undefined;
      return [`${app(ctx)} has no ${ctx.platform} package command`];
    },
  },
  {
    // release-invocation.mjs resolveReleaseBuildInputs: the doctor reports it as its own check, the build as this row.
    id: "build-inputs",
    tier: "files",
    doctorOwned: true,
    run(ctx) {
      const inputs = ctx.buildInputs();
      return inputs.ok ? undefined : [inputs.error.message];
    },
  },
  {
    // The files that can change the packaged app are committed: the doctor reports it as its own check, the build as this row.
    id: "dirty-inputs",
    tier: "files",
    doctorOwned: true,
    after: ["build-inputs"],
    run(ctx) {
      const inputs = ctx.buildInputs();
      if (!inputs.ok) return { skipped: "build-inputs is reported on its own" };
      const dirty = dirtyBuildInputs(ctx.repoRoot, inputs.value.buildInputs, ctx.commit());
      return dirty.length > 0 ? [`dirty files can change the packaged app; resolve them before release:\n${dirty.map((file) => `- ${file}`).join("\n")}`] : undefined;
    },
  },
  {
    // build-release.mjs hashInputs: two inputs may not share a sealed hash key, or one silently replaces the other.
    id: "input-key-collision",
    tier: "files",
    after: ["build-inputs"],
    run(ctx) {
      const inputs = ctx.buildInputs();
      if (!inputs.ok) return { skipped: "build-inputs is reported on its own" };
      const seen = new Set();
      const collided = new Set();
      for (const file of inputs.value.requiredInputs) {
        const key = path.relative(path.dirname(path.dirname(file)), file);
        if (seen.has(key)) collided.add(key);
        seen.add(key);
      }
      return [...collided].map((key) => `release inputs share the hash key ${key}; rename one so every input is sealed`);
    },
  },
  {
    // build-release.mjs assertNsisInPlaceUpgradeContract: the NSIS template that upgrades in place. The build judges it on the target
    // as the config declares it, so a winPackage recipe (which declares no signingContract) is not held to it today.
    id: "nsis-upgrade-contract",
    tier: "files",
    run(ctx) {
      if (ctx.platform !== "win" || ctx.rawTarget?.signingContract !== WINDOWS_NSIS_SIGNING_CONTRACT) return { na: "not a Windows NSIS target declared in the config" };
      try { assertNsisInPlaceUpgradeContract(ctx.root, ctx.rawTarget.nsisUpgradeContract); } catch (error) { return [error.message]; }
      return undefined;
    },
  },
  {
    // release.mjs, after the installer is signed: the post-code-signing minisign needs updater.artifacts to sign.
    id: "win-updater-artifacts",
    tier: "files",
    run(ctx) {
      if (!isNsis(ctx)) return { na: "not a Windows NSIS target" };
      return asList(ctx.target.updater?.artifacts).length > 0 ? undefined : [`${app(ctx)} win must declare updater.artifacts for post-code-signing minisign`];
    },
  },
  {
    // build-release.mjs sealRelease: what the seal needs from the config, evaluated before anything is compiled or signed.
    id: "seal-routes",
    tier: "files",
    run(ctx) {
      const { target, platform, config } = ctx;
      const found = [];
      const declared = asList(target.artifacts);
      for (const entry of declared) {
        const file = typeof entry === "string" ? entry : entry?.file;
        if (typeof file !== "string" || !file) { found.push("target.artifacts entries must be paths or { file }"); break; }
      }
      const installers = asList(target.installer?.artifacts);
      const updaters = asList(target.updater?.artifacts);
      if (installers.length + updaters.length + declared.length === 0) found.push(`nothing to seal: declare installer.artifacts, updater.artifacts or artifacts for the ${platform} target`);
      for (const entry of [...installers, ...updaters]) {
        if (typeof entry?.file !== "string" || !entry.file) { found.push("installer and updater artifacts must each declare a file"); break; }
      }
      const minisign = updaters.length > 0 && target.updater?.kind !== "appinstaller";
      if (minisign) {
        const publicKey = config.distribution?.updaterPublicKey ?? config.distribution?.updaterPubkey ?? target.updater?.pubkey ?? target.updater?.publicKey;
        // A Right Suite app's updater key is the Right Suite contract's (resolveUpdaterTrust); only another app declares its own.
        if (!isSuiteUpdaterApp(config.app) && (typeof publicKey !== "string" || !publicKey.trim())) found.push("updater public key is required to verify updater artifacts before sealing");
        for (const entry of updaters) {
          if (typeof entry?.signature !== "string" || !entry.signature) found.push(`updater artifact ${entry?.file} must declare the signature file sign-updater writes next to it`);
        }
      }
      const github = config.distribution?.provider === "github-releases";
      for (const entry of updaters) {
        if (entry?.key === undefined && !github) found.push(`updater artifact ${entry?.file} must declare the R2 route key (key); only github-releases names the route from the file`);
      }
      // The R2 keys publish-update.mjs checks at upload, tier by tier. Only a private release (r2-rightapps) is held to them. Both
      // tiers check each updater's current key; only the patch tier checks the installer keys and where the patch updater lands, and
      // it cannot run without installer.artifacts ("patch has no installer.artifacts"). The build is tier-neutral, so the patch
      // checks apply exactly when the config makes the patch tier possible.
      if (config.distribution?.provider === "r2-rightapps") {
        const patchTier = installers.length > 0;
        if (patchTier) {
          for (const entry of installers) {
            // publish-update.mjs assertCurrentKey tests the key as it is, so an installer with no key is refused with "got: undefined".
            if (!CURRENT_INSTALLER_KEY.test(entry?.key)) found.push(`installer key must target the replaceable current R2 object, got: ${entry?.key}`);
            if (entry?.versionedKey !== undefined && !VERSIONED_INSTALLER_KEY.test(entry.versionedKey)) found.push(`installer versionedKey must be <app>/installers/<os>/versions/<version>/<file>, got: ${entry.versionedKey}`);
          }
        }
        for (const entry of updaters) {
          if (typeof entry.key !== "string") continue;
          if (!CURRENT_UPDATER_KEY.test(entry.key)) found.push(`updater key must target the replaceable current R2 object, got: ${entry.key}`);
          if (!patchTier) continue;
          // publish-update.mjs patchInstallerKey: the patch updater is uploaded as an installer, under patchKey or the updater key's installers twin.
          const patchKey = entry.patchKey ? entry.patchKey : entry.key.includes("/updates/") ? entry.key.replace("/updates/", "/installers/") : null;
          if (patchKey === null) found.push(`patch updater key must be convertible to installers/current, got: ${entry.key}`);
          else if (!CURRENT_INSTALLER_KEY.test(patchKey)) found.push(`installer key must target the replaceable current R2 object, got: ${patchKey}`);
        }
      }
      // The Android and iOS seals pin one file of target.artifacts (the AAB, the IPA).
      const listed = declared.map((entry) => path.posix.normalize(String(typeof entry === "string" ? entry : entry?.file).replaceAll("\\", "/")));
      if (platform === "android") {
        let aab = null;
        try { aab = validateAndroidTarget(target, { root: ctx.root }).aab?.file ?? null; } catch { /* the android-target check reports a bad target */ }
        if (aab && !listed.includes(path.posix.normalize(aab.replaceAll("\\", "/")))) found.push("android seal requires targets.android.artifacts to list aab.file");
      }
      if (platform === "ios") {
        const ipa = target.recipe?.spec?.ipa;
        if (typeof ipa === "string" && !listed.includes(path.posix.normalize(ipa.replaceAll("\\", "/")))) found.push("ios seal requires targets.ios.artifacts to list iosPackage.ipa");
      }
      return found;
    },
  },
  {
    // publish-update.mjs: the provider the upload takes, for the win and mac releases it uploads (iOS and Android go through their own
    // lanes, whose providers are app-store-connect and google-play). A config that declares no distribution is refused there too, as
    // is a provider it does not know; preflight G16 refuses an unknown provider in the config text.
    id: "distribution-provider",
    tier: "files",
    needsTarget: false,
    run({ config, platform }) {
      if (platform !== "win" && platform !== "mac") return { na: "publish-update.mjs uploads win and mac releases; other platforms upload through their own lane" };
      const distribution = config.distribution;
      const provider = distribution?.provider;
      if (!DISTRIBUTION_PROVIDERS.includes(provider)) return [`release config distribution.provider must be ${DISTRIBUTION_PROVIDERS.join(" or ")}${provider === undefined ? "" : ` (got ${JSON.stringify(provider)})`}`];
      if (provider === "github-releases") {
        try { assertGitHubRepository(distribution.repository); } catch (error) { return [error.message]; }
      }
      return undefined;
    },
  },
  {
    // release.mjs frontendBundleDirs: the mock scan locates the frontend bundle from src-tauri/tauri.conf.json.
    id: "frontend-config-readable",
    tier: "files",
    run(ctx) {
      if (Array.isArray(ctx.target.webBundleScan)) return { na: "target.webBundleScan names the bundle directories" };
      const confPath = path.join(ctx.root, "src-tauri", "tauri.conf.json");
      if (!ctx.exists(confPath)) return { na: "no src-tauri/tauri.conf.json" };
      try { JSON.parse(ctx.fs.readFileSync(confPath, "utf8")); } catch (error) {
        return [`src-tauri/tauri.conf.json is unreadable, so the frontend bundle cannot be located for the mock scan: ${error.message}`];
      }
      return undefined;
    },
  },
  {
    // release-checks.mjs: every config.checks entry runs `<packageManager> run <entry>` after the install; a script that is not
    // in package.json fails there, minutes in.
    id: "declared-checks-exist",
    tier: "files",
    needsTarget: false,
    run(ctx) {
      const checks = asList(ctx.config.checks).filter((script) => script !== false);
      if (checks.length === 0) return { na: "the config declares no checks" };
      if (ctx.skipChecks) return { na: "the build was asked to skip the declared checks" };
      const file = path.join(workdirOf(ctx), "package.json");
      let scripts;
      try { scripts = JSON.parse(ctx.fs.readFileSync(file, "utf8")).scripts ?? {}; } catch {
        return [`config.checks declares ${checks.map((script) => JSON.stringify(script)).join(", ")} but ${file} is missing or unreadable`];
      }
      return checks.filter((script) => typeof script !== "string" || !Object.hasOwn(scripts, script))
        .map((script) => `config.checks names ${JSON.stringify(script)}, which is not a script in ${path.relative(ctx.root, file).replaceAll("\\", "/") || "package.json"}`);
    },
  },
  {
    // release.mjs: a mediaSidecar or externalBin on macOS is verified inside the built .app, so the signing set must name one.
    id: "sidecar-app-entry",
    tier: "files",
    run(ctx) {
      if (ctx.platform !== "mac" || (ctx.target.mediaSidecar === undefined && ctx.target.externalBin === undefined)) return { na: "no mac sidecar" };
      const hasApp = asList(ctx.target.sign?.prePackageFiles).some((entry) => /\.app\/?$/.test(typeof entry === "string" ? entry : String(entry?.file ?? "")));
      if (hasApp) return undefined;
      return [ctx.target.mediaSidecar !== undefined
        ? "mediaSidecar on macOS needs an .app entry in sign.prePackageFiles"
        : "externalBin on macOS needs an .app entry in sign.prePackageFiles"];
    },
  },
  {
    // release.mjs prepareMediaSidecar: the sidecar plan is made from the catalog before anything is staged or compiled. The two parts
    // the committed files decide: the app has a native Cargo manifest to resolve the catalog from, and the catalog the config names
    // plans (a row for each tool, approved, with its digest and notices). A catalog read through `cargo metadata` is the host row
    // media-sidecar-catalog. An invalid mediaSidecar config is the media-sidecar rule's finding, not this row's.
    id: "media-sidecar-plan",
    tier: "files",
    after: ["build-inputs"],
    run(ctx) {
      if (ctx.target.mediaSidecar === undefined || (ctx.platform !== "mac" && ctx.platform !== "win")) return { na: "the target declares no mediaSidecar" };
      let sidecar;
      try { sidecar = validateMediaSidecarConfig(ctx.target.mediaSidecar, { platform: ctx.platform }); } catch { return { skipped: "an invalid mediaSidecar configuration is reported by the media-sidecar rule" }; }
      const inputs = ctx.buildInputs();
      if (!inputs.ok) return { skipped: "build-inputs is reported on its own" };
      const layout = inputs.value.nativeLayout;
      // Unreachable for iOS (mediaSidecar is mac and win only); a Cargo-less layout must still never be dereferenced.
      if (!layout) return [`${app(ctx)} ${ctx.platform} mediaSidecar needs a native Cargo manifest to read the sidecar catalog, and this app has none`];
      if (!sidecar.catalog) return { skipped: "the catalog is resolved through cargo metadata, which the media-sidecar-catalog host row asks" };
      try {
        const loaded = loadSidecarCatalog({ config: sidecar, root: ctx.root, manifestPath: layout.manifestPath, read: (file) => ctx.fs.readFileSync(file) });
        planMediaSidecar({ config: sidecar, ...loaded, root: ctx.root });
      } catch (error) { return [`${app(ctx)} ${ctx.platform} mediaSidecar: [${error.code ?? "ERROR"}] ${error.message}`]; }
      return undefined;
    },
  },
  {
    // build-release.mjs runConfiguredReleaseCheck: the preSeal check runs a command.
    id: "pre-seal-command",
    tier: "files",
    run(ctx) {
      if (!ctx.target.preSeal) return { na: "no preSeal check" };
      return typeof ctx.target.preSeal.cmd === "string" && ctx.target.preSeal.cmd.trim() ? undefined : ["release preSeal check must declare a command"];
    },
  },
]);
