import type {
  ConstitutionDecision,
  ConstitutionDecisionVerdict,
  DaemonConfig,
  FeatureSpec,
  FeatureTask,
  SpecAnalysisRun,
  SpecChecklistResponse,
  SpecClarifyResponse,
  SpecEnvelope,
  SpecImplementResponse,
  SpecPlanResponse,
  SpecTrace,
} from "../types.js";
import { requestDaemonApi } from "../generated/daemon-openapi.js";

export interface CreateSpecRequest {
  workspaceRoot: string;
  title?: string;
  description: string;
  branch?: string;
  userStories?: {
    id?: string;
    priority?: number;
    title: string;
    narrative: string;
    independentTest: string;
  }[];
  functionalRequirements?: {
    id?: string;
    text: string;
    sourceStoryId?: string;
    needsClarification?: boolean;
  }[];
  successCriteria?: {
    id?: string;
    text: string;
    buildable?: boolean;
  }[];
  assumptions?: string[];
  edgeCases?: string[];
}

export interface AddSpecTaskInput {
  id?: string;
  storyId?: string;
  requirementIds?: string[];
  successCriterionIds?: string[];
  foundationPhase?: string;
  description: string;
  paths?: string[];
  parallelizable?: boolean;
  status?: FeatureTask["status"];
  evidenceIds?: string[];
}

export interface RecordSpecConstitutionDecisionRequest {
  id?: string;
  sourcePath?: string;
  sourceSha256?: string;
  rule: string;
  verdict: ConstitutionDecisionVerdict;
  rationale: string;
  evidenceIds?: string[];
}

export async function listSpecs(
  config: DaemonConfig,
  workspaceRoot?: string,
): Promise<FeatureSpec[]> {
  return requestDaemonApi<FeatureSpec[]>(config, "getSpecs", {
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
  });
}

export async function createSpec(
  config: DaemonConfig,
  body: CreateSpecRequest,
): Promise<FeatureSpec> {
  return requestDaemonApi<FeatureSpec, CreateSpecRequest>(config, "postSpecs", { body });
}

export async function getSpec(config: DaemonConfig, specId: string): Promise<SpecEnvelope> {
  return requestDaemonApi<SpecEnvelope>(config, "getSpecsById", {
    pathParams: { id: specId },
  });
}

export async function addSpecTasks(
  config: DaemonConfig,
  specId: string,
  tasks: AddSpecTaskInput[],
): Promise<FeatureTask[]> {
  return requestDaemonApi<FeatureTask[]>(config, "postSpecsByIdTasks", {
    pathParams: { id: specId },
    body: { tasks },
  });
}

export async function listSpecConstitutionDecisions(
  config: DaemonConfig,
  specId: string,
): Promise<ConstitutionDecision[]> {
  return requestDaemonApi<ConstitutionDecision[]>(config, "getSpecsByIdConstitutionDecisions", {
    pathParams: { id: specId },
  });
}

export async function recordSpecConstitutionDecision(
  config: DaemonConfig,
  specId: string,
  body: RecordSpecConstitutionDecisionRequest,
): Promise<ConstitutionDecision> {
  return requestDaemonApi<ConstitutionDecision, RecordSpecConstitutionDecisionRequest>(
    config,
    "postSpecsByIdConstitutionDecisions",
    {
      pathParams: { id: specId },
      body,
    },
  );
}

export async function clarifySpec(
  config: DaemonConfig,
  specId: string,
): Promise<SpecClarifyResponse> {
  return requestDaemonApi<SpecClarifyResponse>(config, "postSpecsByIdClarify", {
    pathParams: { id: specId },
  });
}

export async function planSpec(
  config: DaemonConfig,
  specId: string,
  body: { constraints?: string; includeVerification?: boolean } = {},
): Promise<SpecPlanResponse> {
  return requestDaemonApi<SpecPlanResponse>(config, "postSpecsByIdPlan", {
    pathParams: { id: specId },
    body,
  });
}

export async function checklistSpec(
  config: DaemonConfig,
  specId: string,
  body: { focus?: string } = {},
): Promise<SpecChecklistResponse> {
  return requestDaemonApi<SpecChecklistResponse>(config, "postSpecsByIdChecklist", {
    pathParams: { id: specId },
    body,
  });
}

export async function analyzeSpec(config: DaemonConfig, specId: string): Promise<SpecAnalysisRun> {
  return requestDaemonApi<SpecAnalysisRun>(config, "postSpecsByIdAnalyze", {
    pathParams: { id: specId },
  });
}

export async function implementSpec(
  config: DaemonConfig,
  specId: string,
  body: { title?: string; dryRun?: boolean } = {},
): Promise<SpecImplementResponse> {
  return requestDaemonApi<SpecImplementResponse>(config, "postSpecsByIdImplement", {
    pathParams: { id: specId },
    body,
  });
}

export async function getSpecTrace(config: DaemonConfig, specId: string): Promise<SpecTrace> {
  return requestDaemonApi<SpecTrace>(config, "getSpecsByIdTrace", {
    pathParams: { id: specId },
  });
}
