import type { AppId } from "./provider-types";

export function resolveProviderIcon(
  appId: AppId,
  icon?: string,
  iconColor?: string,
): string | undefined {
  const normalizedIcon = icon?.trim();
  if (!normalizedIcon) return undefined;
  if (appId === "grokbuild" && normalizedIcon === "grok" && !iconColor?.trim())
    return undefined;
  return normalizedIcon;
}
