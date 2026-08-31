import { invoke } from "@tauri-apps/api/core";

import type { SkillRecord } from "./skill-types";

export const skillsApi = {
  list: () => invoke<SkillRecord[]>("list_installed_skills"),
  toggle: (skillId: string, appId: string, enabled: boolean) =>
    invoke<void>("toggle_skill_app", { skillId, appId, enabled }),
};
