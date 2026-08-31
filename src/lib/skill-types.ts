import type { AppId } from "./provider-types";

export interface SkillRecord {
  id: string;
  name: string;
  description?: string;
  directory: string;
  repoOwner?: string;
  repoName?: string;
  apps: Record<AppId, boolean>;
  issue?: string;
  appIssues?: Partial<Record<AppId, string>>;
}
