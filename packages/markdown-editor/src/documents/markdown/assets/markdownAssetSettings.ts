/**
 * Per-document Markdown asset settings.
 *
 * Settings persist in the existing settings store. They control
 * how the asset folder is computed, what gets inlined vs. saved,
 * and whether the smart-paste path is enabled.
 */

export interface MarkdownAssetSettings {
  /**
   * Where dropped/pasted assets land. `"auto"` means:
   * `<documentDir>/<basename>.assets/`. A string overrides the
   * folder name (still relative to the document).
   */
  readonly assetsFolder: "auto" | string;
  /**
   * File size threshold (in MiB) above which pasted images are
   * NOT inlined as data URLs and ARE saved as files. Defaults to
   * 1 MiB.
   */
  readonly autoFileThresholdMiB: number;
  /**
   * When true, the smart-paste path strips nested spans and
   * inlines tables. Defaults to true.
   */
  readonly smartPasteEnabled: boolean;
  /**
   * When true, the smart-paste path keeps bold/italic markers
   * from the source clipboard HTML. Defaults to true.
   */
  readonly smartPasteKeepFormatting: boolean;
  /**
   * When true, smart-paste converts Office-style "MsoNormal"
   * paragraphs into proper Markdown paragraphs. Defaults to true.
   */
  readonly smartPasteNormalizeMso: boolean;
}

export const DEFAULT_MARKDOWN_ASSET_SETTINGS: MarkdownAssetSettings = {
  assetsFolder: "auto",
  autoFileThresholdMiB: 1,
  smartPasteEnabled: true,
  smartPasteKeepFormatting: true,
  smartPasteNormalizeMso: true,
};

/**
 * Resolved asset folder path. The Rust side canonicalizes the
 * document directory before creating the folder.
 */
export function resolveAssetsFolder(
  settings: MarkdownAssetSettings,
  documentBasename: string,
): string {
  if (settings.assetsFolder === "auto") {
    return `${documentBasename}.assets`;
  }
  return settings.assetsFolder;
}

/**
 * In-memory store. The real persistence layer writes to
 * `tauri::api::settings::get`/`set`; this wrapper exposes a
 * pure-TS store for tests.
 */
export class MarkdownAssetSettingsStore {
  private current: MarkdownAssetSettings;

  constructor(initial: MarkdownAssetSettings = DEFAULT_MARKDOWN_ASSET_SETTINGS) {
    this.current = initial;
  }

  get snapshot(): MarkdownAssetSettings {
    return this.current;
  }

  update(patch: Partial<MarkdownAssetSettings>): void {
    this.current = { ...this.current, ...patch };
  }

  reset(): void {
    this.current = DEFAULT_MARKDOWN_ASSET_SETTINGS;
  }
}
