import { APP_IDS } from "./apps";
import type { AppId } from "./provider-types";

export type Theme = "light" | "dark" | "system";
export type VisibleApps = Record<AppId, boolean>;

export const THEME_STORAGE_KEY = "cc-switch-lite:theme";
export const VISIBLE_APPS_STORAGE_KEY = "cc-switch-lite:visible-apps";

export function initialTheme(): Theme {
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
  return stored === "light" || stored === "dark" || stored === "system"
    ? stored
    : "system";
}

export function allAppsVisible(): VisibleApps {
  return Object.fromEntries(
    APP_IDS.map((appId) => [appId, true]),
  ) as VisibleApps;
}

export function initialVisibleApps(): VisibleApps {
  const defaults = allAppsVisible();
  const stored = window.localStorage.getItem(VISIBLE_APPS_STORAGE_KEY);
  if (!stored) return defaults;

  try {
    const parsed = JSON.parse(stored) as Record<string, unknown>;
    for (const appId of APP_IDS) {
      if (typeof parsed[appId] === "boolean") defaults[appId] = parsed[appId];
    }
  } catch {
    return defaults;
  }

  if (!APP_IDS.some((appId) => defaults[appId])) return allAppsVisible();
  return defaults;
}
