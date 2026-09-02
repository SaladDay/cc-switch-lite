import { useMemo } from "react";

import {
  getIcon,
  getIconMetadata,
  getIconUrl,
  hasIcon,
  isUrlIcon,
} from "../icons/extracted";
import { cn } from "../lib/utils";

interface ProviderIconProps {
  icon?: string;
  name: string;
  color?: string;
  size?: number | string;
  className?: string;
  showFallback?: boolean;
}

export function ProviderIcon({
  icon,
  name,
  color,
  size = 32,
  className,
  showFallback = true,
}: ProviderIconProps) {
  const iconSvg = useMemo(() => {
    if (icon && !isUrlIcon(icon) && hasIcon(icon)) return getIcon(icon);
    return "";
  }, [icon]);
  const iconUrl = useMemo(() => {
    if (icon && isUrlIcon(icon)) return getIconUrl(icon);
    return "";
  }, [icon]);
  const sizeStyle = useMemo(() => {
    const value = typeof size === "number" ? `${size}px` : size;
    return { width: value, height: value, fontSize: value, lineHeight: 1 };
  }, [size]);
  const effectiveColor = useMemo(() => {
    if (color?.trim()) return color;
    if (!icon) return undefined;
    const defaultColor = getIconMetadata(icon)?.defaultColor;
    return defaultColor && defaultColor !== "currentColor"
      ? defaultColor
      : undefined;
  }, [color, icon]);

  if (iconSvg) {
    return (
      <span
        aria-hidden="true"
        className={cn(
          "inline-flex flex-shrink-0 items-center justify-center",
          className,
        )}
        title={name}
        style={{ ...sizeStyle, color: effectiveColor }}
        dangerouslySetInnerHTML={{ __html: iconSvg }}
      />
    );
  }

  if (iconUrl) {
    return (
      <img
        src={iconUrl}
        alt=""
        aria-hidden="true"
        title={name}
        className={cn(
          "inline-flex flex-shrink-0 items-center justify-center object-contain",
          className,
        )}
        style={{ width: sizeStyle.width, height: sizeStyle.height }}
        loading="lazy"
      />
    );
  }

  if (!showFallback) return null;
  const initials = name
    .split(" ")
    .map((word) => word[0])
    .join("")
    .toUpperCase()
    .slice(0, 2);

  return (
    <span
      aria-hidden="true"
      className={cn(
        "inline-flex flex-shrink-0 items-center justify-center rounded-lg",
        "bg-muted font-semibold text-muted-foreground",
        className,
      )}
      title={name}
      style={sizeStyle}
    >
      <span
        style={{
          fontSize:
            typeof size === "number"
              ? `${Math.max(size * 0.5, 12)}px`
              : "0.5em",
        }}
      >
        {initials}
      </span>
    </span>
  );
}
