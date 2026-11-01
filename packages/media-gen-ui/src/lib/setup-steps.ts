import { hostAppName } from "../appName";
/**
 * Setup step definitions (pure data, no imports) so the node test can check them. The view in
 * src/views/Onboarding.tsx attaches each step's body and renders its facts. Every step must carry
 * all four facts; a step without them does not type-check and fails src/tests/setup-steps.test.mjs.
 */

export const SETUP_STEP_IDS = ['library', 'computer', 'installed', 'keys', 'models'] as const;
export type SetupStepId = (typeof SETUP_STEP_IDS)[number];

export interface SetupStepFacts {
  /** (a) What the step is for. */
  purpose: string;
  /** (b) Cost: free, paid by the provider, and any disk or download size the repo states. */
  cost: string;
  /** (c) What skipping the step costs the user: which features stay unavailable. */
  skipCost: string;
  /** (d) Whether the step can be resumed later, and where. */
  resume: string;
}

export interface SetupStepDef {
  id: SetupStepId;
  label: string;
  title: string;
  description?: string;
  skippable?: boolean;
  facts: SetupStepFacts;
}

const RESUME_SETUP = 'Yes. Settings > Setup > Re-open setup…';
const PROVIDER_PRICING = "cost depends on the provider's own pricing";

export const SETUP_STEPS: readonly SetupStepDef[] = [
  {
    id: 'library',
    label: 'Library',
    title: `Welcome to ${hostAppName()}`,
    description: 'One place to generate images, video and voice with cloud APIs, local models, or both. Setup takes a minute and you can skip any step.',
    skippable: true,
    facts: {
      purpose: 'Shows the library folder where generated files and their metadata are saved.',
      cost: 'Free. Nothing is uploaded during setup.',
      skipCost: 'Nothing is lost. The folder is only shown here, and generation still saves to the library.',
      resume: `${RESUME_SETUP} Shows the same library folder.`,
    },
  },
  {
    id: 'computer',
    label: 'This computer',
    title: 'Checking this computer',
    description: 'Used to decide which models can run locally and which go to an API.',
    skippable: true,
    facts: {
      purpose: 'Reads the platform, architecture and local image runtime to decide which models can run on this computer.',
      cost: 'Free. Reads this computer only; nothing is downloaded.',
      skipCost: 'Nothing stops working. The hardware report is not shown during setup; local model choices can still be made later.',
      resume: RESUME_SETUP,
    },
  },
  {
    id: 'installed',
    label: 'Already installed',
    title: 'Already on this computer',
    description: 'Other Orthic Labs apps found on this computer. Their models and settings can be reused instead of downloading them again.',
    skippable: true,
    facts: {
      purpose: 'Finds other Orthic Labs apps on this computer whose models and settings could be reused.',
      cost: 'Free. Only lists the apps; nothing is downloaded or copied here.',
      skipCost: 'Models already on this computer from other apps are not reused, so any model you want downloads again.',
      resume: RESUME_SETUP,
    },
  },
  {
    id: 'keys',
    label: 'API keys',
    title: 'Connect API providers',
    description: 'Keys are stored in your system keychain, never in project files. Add only what you use, or skip and work locally.',
    skippable: true,
    facts: {
      purpose: 'Stores API keys for cloud providers in the system keychain or credential manager.',
      cost: `Free to add a key. Each cloud generation is paid to its provider: ${PROVIDER_PRICING}.`,
      skipCost: 'Cloud generation stays unavailable until a key is added. Local models still work.',
      resume: `${RESUME_SETUP} Keys can also be added directly in Settings > API keys.`,
    },
  },
  {
    id: 'models',
    label: 'Local models',
    title: 'Choose local models',
    facts: {
      purpose: 'Installs local image and voice models that run on this computer, each file checked against its hash.',
      cost: 'Free after install. Download sizes: Qwen3-TTS 0.6B voice is 1.8 GB; the Qwen image profile is about 12.9 GB of files. Each model lists its size before you install it.',
      skipCost: 'Local image and local voice generation stay unavailable until a model is installed. Cloud providers still work if their keys are added.',
      resume: `${RESUME_SETUP} Models can also be installed from Settings > Models.`,
    },
  },
];

/** The definition for one step id; throws if the id is not in SETUP_STEPS. */
export function setupStepDef(id: SetupStepId): SetupStepDef {
  const def = SETUP_STEPS.find((s) => s.id === id);
  if (!def) throw new Error(`No setup step definition for "${id}"`);
  return def;
}
