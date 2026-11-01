export type RecipeInputType = "string" | "number" | "boolean" | "path" | "secret";
export type RecipeRequirementLevel = "required" | "optional";
export type RecipeSource = "local" | "inline" | "github" | "builtIn";
export type RecipeVerdict = "pass" | "warn" | "block";
export type RecipeFindingSeverity = "info" | "warning" | "critical";
export type RecipeRunStatus = "submitted" | "running" | "completed" | "failed";

export interface RecipeParameter {
  key: string;
  inputType: RecipeInputType;
  requirement: RecipeRequirementLevel;
  description: string;
  default?: string | null;
  value?: string | null;
}

export interface RecipeMcpRequirement {
  server: string;
  commands: string[];
}

export interface RetryPolicy {
  maxAttempts: number;
  backoffMs: number;
}

export interface SubRecipeRef {
  id: string;
  params: Record<string, string>;
}

export interface RecipeSettings {
  model?: string | null;
  effort?: string | null;
  workspace?: string | null;
  permissionMode?: string | null;
  toolset?: string | null;
  executionBackend?: string | null;
}

export interface Recipe {
  id?: string | null;
  version: string;
  title: string;
  description: string;
  instructions?: string | null;
  prompt?: string | null;
  activities: string[];
  parameters: RecipeParameter[];
  requiredToolsets: string[];
  requiredMcpServers: RecipeMcpRequirement[];
  requiredPlugins: string[];
  skills: string[];
  settings: RecipeSettings;
  responseSchema?: unknown;
  retry?: RetryPolicy | null;
  subRecipes: SubRecipeRef[];
}

export interface RecipeFinding {
  id: string;
  severity: RecipeFindingSeverity;
  kind: string;
  message: string;
}

export interface RecipeValidationRun {
  id: string;
  recipeId?: string | null;
  verdict: RecipeVerdict;
  findings: RecipeFinding[];
  createdAt: string;
}

export interface InstalledRecipe {
  id: string;
  source: RecipeSource;
  sourceUri?: string | null;
  title: string;
  version: string;
  checksum: string;
  trusted: boolean;
  recipe: Recipe;
  installedAt: string;
  latestValidation?: RecipeValidationRun | null;
}

export interface RenderedRecipe {
  recipeId?: string | null;
  title: string;
  renderedPrompt: string;
  renderedPromptHash: string;
  params: Record<string, string>;
  settings: RecipeSettings;
  requiredToolsets: string[];
  requiredPlugins: string[];
  skills: string[];
}

export interface RecipeRun {
  id: string;
  recipeId: string;
  sessionId: string;
  status: RecipeRunStatus;
  params: Record<string, string>;
  renderedPromptHash: string;
  resultArtifactId?: string | null;
  createdAt: string;
  completedAt?: string | null;
}

export interface RecipeDeeplink {
  recipeId: string;
  action: "run" | "render" | "schedule" | string;
  params: Record<string, string>;
  url: string;
}

export interface RecipeScheduleDraft {
  id: string;
  cronExpr: string;
  timeZone: string;
  workspaceRoot: string;
  workdir: string;
  toolset: string;
  executionBackend: string;
  skills: string;
  deliveryKind:
    | "desktop_notification"
    | "file_artifact"
    | "session_message"
    | "webhook"
    | "ntfy"
    | "telegram_chat"
    | "none";
  deliveryTarget: string;
  paused: boolean;
}

export type LocalModelFitVerdict = "good" | "caution" | "unknown" | "tooLarge";

export interface LocalRuntimeStatus {
  id: string;
  name: string;
  detected: boolean;
  version?: string | null;
  installHint: string;
}

export interface LocalGpuInfo {
  name: string;
  vendor?: string | null;
  memoryMb?: number | null;
  source: string;
}

export interface LocalModelHardwareReport {
  cpuThreads: number;
  totalMemoryMb?: number | null;
  gpuDevices: LocalGpuInfo[];
  gpuNotes: string[];
  runtimes: LocalRuntimeStatus[];
}

export interface LocalModelRecommendation {
  id: string;
  name: string;
  providerRuntime: string;
  modelSlug: string;
  minMemoryMb: number;
  recommendedMemoryMb: number;
  minVramMb?: number | null;
  recommendedVramMb?: number | null;
  detectedVramMb?: number | null;
  fitConfidence: string;
  quantizationNotes: string;
  toolCallReliability: string;
  fit: LocalModelFitVerdict;
  fitReason: string;
  endpointHint: string;
}

export interface LocalModelCatalog {
  hardware: LocalModelHardwareReport;
  recommendations: LocalModelRecommendation[];
}

export interface LocalModelProbeResult {
  ok: boolean;
  baseUrl: string;
  modelsUrl: string;
  modelCount: number;
  models: string[];
  error?: string | null;
}

export interface LocalModelChatSmokeResult {
  ok: boolean;
  baseUrl: string;
  chatUrl: string;
  model: string;
  responsePreview?: string | null;
  error?: string | null;
}

export interface LocalProviderRouteSuggestion {
  providerId: string;
  displayName: string;
  baseUrl: string;
  model: string;
  apiKeyRequired: boolean;
  note: string;
  applied: boolean;
  setDefault: boolean;
  configPath: string;
}
