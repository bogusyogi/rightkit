export type UpdateTier = 'patch' | 'update';

export interface UpdateHostContractVector {
  name: string;
  tier: UpdateTier;
  pro: boolean;
  eligible: boolean;
  reason: 'pro_required' | null;
}

/** Framework-neutral fixtures every app adapter must run before Phase 3 publication. */
export const UPDATE_HOST_CONTRACT_VECTORS: readonly UpdateHostContractVector[] = [
  { name: 'free patch', tier: 'patch', pro: false, eligible: true, reason: null },
  { name: 'pro patch', tier: 'patch', pro: true, eligible: true, reason: null },
  { name: 'free feature update', tier: 'update', pro: false, eligible: false, reason: 'pro_required' },
  { name: 'pro feature update', tier: 'update', pro: true, eligible: true, reason: null },
];
