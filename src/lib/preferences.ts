export type Theme = "light" | "dark" | "system";
export type AppVisibility = Record<string, boolean>;

export const THEME_STORAGE_KEY = "cc-switch-lite:theme";
export const APP_VISIBILITY_STORAGE_KEY = "cc-switch-lite:visible-apps";

export function initialTheme(): Theme {
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
  return stored === "light" || stored === "dark" || stored === "system"
    ? stored
    : "system";
}

export function initialAppVisibility(): AppVisibility {
  const stored = window.localStorage.getItem(APP_VISIBILITY_STORAGE_KEY);
  if (!stored) return {};
  try {
    const parsed = JSON.parse(stored) as unknown;
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
      return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(
        ([appId, visible]) => appId.trim() && typeof visible === "boolean",
      ),
    );
  } catch {
    return {};
  }
}

export function appIsVisible(
  visibility: AppVisibility,
  appId: string,
): boolean {
  return visibility[appId] !== false;
}
