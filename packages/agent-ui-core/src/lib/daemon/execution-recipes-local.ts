import type {
  BackendDoctorReport,
  CreateExecutionWorkspaceResponse,
  DaemonConfig,
  ExecutionBackendRegistry,
  ExecutionProcessListResponse,
  ExecutionProcessLogsResponse,
  ExecutionProcessResponse,
  ExecutionWorkspaceListResponse,
  InstalledRecipe,
  LocalModelCatalog,
  LocalModelChatSmokeResult,
  LocalModelHardwareReport,
  LocalModelProbeResult,
  LocalProviderRouteSuggestion,
  Recipe,
  RecipeDeeplink,
  RecipeRun,
  RecipeValidationRun,
  RenderedRecipe,
  ScheduleRecipeRequest,
  ScheduleRecipeResponse,
  WriteExecutionProcessStdinRequest,
} from "../types";
import { requestDaemonApi } from "../generated/daemon-openapi";
import type { SaveExecutionNetworkProfileRequest } from "./config-host";

export async function listExecutionBackends(
  config: DaemonConfig,
  workspaceRoot?: string,
): Promise<ExecutionBackendRegistry> {
  return requestDaemonApi<ExecutionBackendRegistry>(config, "getExecutionBackends", {
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
  });
}

export async function setDefaultExecutionBackend(
  config: DaemonConfig,
  backendId: string,
  workspaceRoot?: string,
): Promise<ExecutionBackendRegistry> {
  return requestDaemonApi<ExecutionBackendRegistry>(config, "patchExecutionBackendsDefault", {
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
    body: { backendId },
  });
}

export async function updateExecutionBackend(
  config: DaemonConfig,
  backendId: string,
  body: {
    enabled?: boolean;
    resourceLimits?: {
      cpu?: number | null;
      memoryMb?: number | null;
      diskMb?: number | null;
    };
    networkProfile?: string | null;
  },
  workspaceRoot?: string,
): Promise<ExecutionBackendRegistry> {
  return requestDaemonApi<ExecutionBackendRegistry, typeof body>(
    config,
    "patchExecutionBackendsById",
    {
      pathParams: { id: backendId },
      query: { workspaceRoot: workspaceRoot?.trim() || undefined },
      body,
    },
  );
}

export async function saveExecutionNetworkProfile(
  config: DaemonConfig,
  profileId: string,
  body: SaveExecutionNetworkProfileRequest,
  workspaceRoot?: string,
): Promise<ExecutionBackendRegistry> {
  return requestDaemonApi<ExecutionBackendRegistry, SaveExecutionNetworkProfileRequest>(
    config,
    "putExecutionNetworkProfilesById",
    {
      pathParams: { id: profileId },
      query: { workspaceRoot: workspaceRoot?.trim() || undefined },
      body,
    },
  );
}

export async function deleteExecutionNetworkProfile(
  config: DaemonConfig,
  profileId: string,
  workspaceRoot?: string,
): Promise<ExecutionBackendRegistry> {
  return requestDaemonApi<ExecutionBackendRegistry>(config, "deleteExecutionNetworkProfilesById", {
    pathParams: { id: profileId },
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
  });
}

export async function doctorExecutionBackend(
  config: DaemonConfig,
  backendId: string,
  workspaceRoot?: string,
): Promise<BackendDoctorReport> {
  return requestDaemonApi<BackendDoctorReport>(config, "postExecutionBackendsByIdDoctor", {
    pathParams: { id: backendId },
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
  });
}

export async function listExecutionWorkspaces(
  config: DaemonConfig,
  workspaceRoot?: string,
): Promise<ExecutionWorkspaceListResponse> {
  return requestDaemonApi<ExecutionWorkspaceListResponse>(config, "getExecutionWorkspaces", {
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
  });
}

export async function createExecutionWorkspace(
  config: DaemonConfig,
  body: {
    backendId: string;
    workspaceId?: string;
    name?: string;
  },
  workspaceRoot?: string,
): Promise<CreateExecutionWorkspaceResponse> {
  return requestDaemonApi<CreateExecutionWorkspaceResponse, typeof body>(
    config,
    "postExecutionWorkspaces",
    {
      query: { workspaceRoot: workspaceRoot?.trim() || undefined },
      body,
    },
  );
}

export async function listExecutionProcesses(
  config: DaemonConfig,
  workspaceRoot?: string,
): Promise<ExecutionProcessListResponse> {
  return requestDaemonApi<ExecutionProcessListResponse>(config, "getExecutionProcesses", {
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
  });
}

export async function createExecutionProcess(
  config: DaemonConfig,
  body: {
    sessionId: string;
  },
): Promise<ExecutionProcessResponse> {
  return requestDaemonApi<ExecutionProcessResponse, typeof body>(config, "postExecutionProcesses", {
    body,
  });
}

export async function getExecutionProcess(
  config: DaemonConfig,
  processId: string,
): Promise<ExecutionProcessResponse> {
  return requestDaemonApi<ExecutionProcessResponse>(config, "getExecutionProcessesById", {
    pathParams: { id: processId },
  });
}

export async function getExecutionProcessLogs(
  config: DaemonConfig,
  processId: string,
  tailBytes?: number,
): Promise<ExecutionProcessLogsResponse> {
  return requestDaemonApi<ExecutionProcessLogsResponse>(config, "getExecutionProcessesByIdLogs", {
    pathParams: { id: processId },
    query: {
      tailBytes: tailBytes && tailBytes > 0 ? Math.floor(tailBytes) : undefined,
    },
  });
}

export async function killExecutionProcess(
  config: DaemonConfig,
  processId: string,
): Promise<ExecutionProcessResponse> {
  return requestDaemonApi<ExecutionProcessResponse>(config, "postExecutionProcessesByIdKill", {
    pathParams: { id: processId },
  });
}

export async function writeExecutionProcessStdin(
  config: DaemonConfig,
  processId: string,
  body: WriteExecutionProcessStdinRequest,
): Promise<ExecutionProcessResponse> {
  return requestDaemonApi<ExecutionProcessResponse, WriteExecutionProcessStdinRequest>(
    config,
    "postExecutionProcessesByIdStdin",
    {
      pathParams: { id: processId },
      body,
    },
  );
}

export async function listRecipes(config: DaemonConfig): Promise<InstalledRecipe[]> {
  return requestDaemonApi<InstalledRecipe[]>(config, "getRecipes");
}

export async function installRecipe(
  config: DaemonConfig,
  body: {
    recipe: Recipe;
    source?: "local" | "inline" | "github" | "builtIn";
    sourceUri?: string;
    trusted?: boolean;
  },
): Promise<InstalledRecipe> {
  return requestDaemonApi<InstalledRecipe, typeof body>(config, "postRecipesInstall", { body });
}

export async function installRecipeFile(
  config: DaemonConfig,
  body: { path: string; trusted?: boolean },
): Promise<InstalledRecipe> {
  return requestDaemonApi<InstalledRecipe, typeof body>(config, "postRecipesInstallFile", { body });
}

export async function installRecipeUrl(
  config: DaemonConfig,
  body: { url: string; trusted?: boolean },
): Promise<InstalledRecipe> {
  return requestDaemonApi<InstalledRecipe, typeof body>(config, "postRecipesInstallUrl", { body });
}

export async function validateRecipe(
  config: DaemonConfig,
  recipe: Recipe,
): Promise<RecipeValidationRun> {
  return requestDaemonApi<RecipeValidationRun>(config, "postRecipesValidate", { body: { recipe } });
}

export async function renderRecipe(
  config: DaemonConfig,
  body: { recipeId?: string; recipe?: Recipe; params?: Record<string, string> },
): Promise<RenderedRecipe> {
  return requestDaemonApi<RenderedRecipe, typeof body>(config, "postRecipesRender", {
    body,
  });
}

export async function createRecipeDeeplink(
  config: DaemonConfig,
  recipeId: string,
  body: { action?: "run" | "render" | "schedule" | string; params?: Record<string, string> } = {},
): Promise<RecipeDeeplink> {
  return requestDaemonApi<RecipeDeeplink, typeof body>(config, "postRecipesByIdDeeplink", {
    pathParams: { id: recipeId },
    body,
  });
}

export async function parseRecipeDeeplink(
  config: DaemonConfig,
  url: string,
): Promise<RecipeDeeplink> {
  return requestDaemonApi<RecipeDeeplink>(config, "postRecipesDeeplinkParse", { body: { url } });
}

export async function runRecipe(
  config: DaemonConfig,
  recipeId: string,
  body: { params?: Record<string, string>; workspaceRoot?: string } = {},
): Promise<RecipeRun> {
  return requestDaemonApi<RecipeRun, typeof body>(config, "postRecipesByIdRun", {
    pathParams: { id: recipeId },
    body,
  });
}

export async function scheduleRecipe(
  config: DaemonConfig,
  recipeId: string,
  body: ScheduleRecipeRequest,
): Promise<ScheduleRecipeResponse> {
  return requestDaemonApi<ScheduleRecipeResponse, ScheduleRecipeRequest>(
    config,
    "postRecipesByIdSchedule",
    { pathParams: { id: recipeId }, body },
  );
}

export async function listRecipeRuns(config: DaemonConfig, recipeId: string): Promise<RecipeRun[]> {
  return requestDaemonApi<RecipeRun[]>(config, "getRecipesByIdRuns", {
    pathParams: { id: recipeId },
  });
}

export async function getLocalModelHardware(
  config: DaemonConfig,
): Promise<LocalModelHardwareReport> {
  return requestDaemonApi<LocalModelHardwareReport>(config, "getLocalModelsHardware");
}

export async function getLocalModelCatalog(config: DaemonConfig): Promise<LocalModelCatalog> {
  return requestDaemonApi<LocalModelCatalog>(config, "getLocalModelsCatalog");
}

export async function probeLocalModelEndpoint(
  config: DaemonConfig,
  baseUrl: string,
): Promise<LocalModelProbeResult> {
  return requestDaemonApi<LocalModelProbeResult>(config, "postLocalModelsProbe", {
    body: { baseUrl },
  });
}

export async function smokeLocalModelChat(
  config: DaemonConfig,
  body: { baseUrl: string; model: string },
): Promise<LocalModelChatSmokeResult> {
  return requestDaemonApi<LocalModelChatSmokeResult, typeof body>(
    config,
    "postLocalModelsChatSmoke",
    { body },
  );
}

export async function suggestLocalModelProvider(
  config: DaemonConfig,
  body: {
    providerId: string;
    baseUrl: string;
    model: string;
    setDefault?: boolean;
    workspaceRoot?: string;
  },
): Promise<LocalProviderRouteSuggestion> {
  return requestDaemonApi<LocalProviderRouteSuggestion>(config, "postLocalModelsAddProvider", {
    query: { workspaceRoot: body.workspaceRoot?.trim() || undefined },
    body: {
      providerId: body.providerId,
      baseUrl: body.baseUrl,
      model: body.model,
      setDefault: body.setDefault ?? false,
    },
  });
}
