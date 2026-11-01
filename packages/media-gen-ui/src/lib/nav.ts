export type ViewId = 'kdp' | 'production' | 'image' | 'video' | 'avatars' | 'motion' | 'voice' | 'voices' | 'recipes' | 'swap' | 'gallery' | 'jobs' | 'settings' | 'music' | 'sfx' | 'upscale' | 'lipsync';

export const VIEW_TITLES: Record<ViewId, string> = {
  kdp: 'KDP',
  production: 'Production',
  image: 'Image',
  video: 'Video',
  avatars: 'Avatars',
  motion: 'Motion',
  voice: 'Voice',
  voices: 'Voice library',
  recipes: 'Recipes',
  swap: 'Template swap',
  gallery: 'Gallery',
  music: 'Music',
  sfx: 'Sound FX',
  upscale: 'Upscale',
  lipsync: 'Lipsync',
  jobs: 'Jobs',
  settings: 'Settings'
};

/** The Settings sections by id, in the order the Settings nav lists them. */
export const SETTINGS_SECTION_IDS = ['appearance', 'setup', 'legal', 'api-keys', 'models', 'spend', 'engine', 'about'] as const;
export type SettingsSectionId = (typeof SETTINGS_SECTION_IDS)[number];

/** The requested Settings section when it exists, else undefined, so Settings opens its default section. */
export function settingsSectionOrDefault(id: string | null | undefined): SettingsSectionId | undefined {
  return (SETTINGS_SECTION_IDS as readonly string[]).includes(id ?? '') ? (id as SettingsSectionId) : undefined;
}
