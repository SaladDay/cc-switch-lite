import type { AppId } from "./provider-types";

export type SkillControlReason =
  | "missingSource"
  | "invalidSource"
  | "recoveryPending"
  | "managedReferenceDrift"
  | "catalogDrift"
  | "nativeConflict"
  | "unifiedConflict"
  | "observationFailed"
  | "invalidConfiguration"
  | "directUnifiedDiscovery"
  | "required"
  | "globallyDisabled"
  | "externallyDisabled";

export interface SkillAppState {
  app: AppId;
  selected: boolean | null;
  enabled: boolean | null;
  writable: boolean;
  canEnable: boolean;
  canDisable: boolean;
  reason: SkillControlReason | null;
}

export interface InstalledSkill {
  id: string;
  name: string;
  description?: string;
  directory: string;
  apps: SkillAppState[];
}
