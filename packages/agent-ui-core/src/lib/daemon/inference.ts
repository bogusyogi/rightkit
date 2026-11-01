import type { DaemonConfig } from "../types";
import type {
  CatalogGroup,
  DirectConnection,
  InferenceAuthority,
  LoadSnapshot,
} from "../types/inference";
import { requestDaemonApi } from "../generated/daemon-openapi";

export async function listInferenceConnections(config: DaemonConfig): Promise<DirectConnection[]> {
  const body = await requestDaemonApi<{ connections: DirectConnection[] }>(config, "getInferenceConnections");
  return body.connections;
}
export async function listInferenceCatalog(config: DaemonConfig): Promise<CatalogGroup[]> {
  const body = await requestDaemonApi<{ groups: CatalogGroup[] }>(config, "getInferenceCatalog");
  return body.groups;
}
export const getInferenceLoad = (config: DaemonConfig) => requestDaemonApi<LoadSnapshot>(config, "getInferenceLoad");
export const createDirectConnection = (config: DaemonConfig, body: Omit<DirectConnection, "id" | "credentialConfigured" | "credentialRevision" | "configurationRevision" | "createdAt" | "updatedAt">) => requestDaemonApi<DirectConnection, typeof body>(config, "postInferenceConnectionsDirect", { body });
export const updateDirectConnection = (config: DaemonConfig, id: string, body: Partial<DirectConnection>) => requestDaemonApi<DirectConnection, typeof body>(config, "putInferenceConnectionsDirectById", { pathParams: { id }, body });
export const deleteDirectConnection = (config: DaemonConfig, id: string) => requestDaemonApi<void>(config, "deleteInferenceConnectionsDirectById", { pathParams: { id } });
export const setDirectConnectionSecret = (config: DaemonConfig, id: string, secret: string) => requestDaemonApi<Pick<DirectConnection, "credentialConfigured" | "credentialRevision">, { secret: string }>(config, "putInferenceConnectionsDirectByIdSecret", { pathParams: { id }, body: { secret } });
export const deleteDirectConnectionSecret = (config: DaemonConfig, id: string) => requestDaemonApi<void>(config, "deleteInferenceConnectionsDirectByIdSecret", { pathParams: { id } });
export const probeInferenceConnection = (config: DaemonConfig, authority: InferenceAuthority, id: string) => requestDaemonApi<CatalogGroup>(config, "postInferenceConnectionsByAuthorityByIdProbe", { pathParams: { authority, id } });
