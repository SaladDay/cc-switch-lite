import type { AppId } from "./provider-types";

export interface SkillAppState {
  enabled: boolean | null;
  issue?: string;
}

export interface SkillRecord {
  id: string;
  name: string;
  description?: string;
  directory: string;
  repoOwner?: string;
  repoName?: string;
  apps: Partial<Record<AppId, SkillAppState>>;
  issue?: string;
}
