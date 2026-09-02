import { invoke } from "@tauri-apps/api/core";

import type { InstalledSkill } from "./skill-types";

export const skillsApi = {
  list: () => invoke<InstalledSkill[]>("list_skills"),
  toggle: (skillId: string, appId: string, enabled: boolean) =>
    invoke<void>("toggle_skill_app", { skillId, appId, enabled }),
};
