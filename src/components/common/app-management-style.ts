const APP_ACTIVE_CLASSES: Record<string, string> = {
  claude:
    "bg-orange-500/10 ring-1 ring-orange-500/20 hover:bg-orange-500/20 text-orange-600 dark:text-orange-400",
  "claude-desktop":
    "bg-amber-500/10 ring-1 ring-amber-500/20 hover:bg-amber-500/20 text-amber-700 dark:text-amber-300",
  codex:
    "bg-green-500/10 ring-1 ring-green-500/20 hover:bg-green-500/20 text-green-600 dark:text-green-400",
  gemini:
    "bg-blue-500/10 ring-1 ring-blue-500/20 hover:bg-blue-500/20 text-blue-600 dark:text-blue-400",
  grokbuild:
    "bg-cyan-500/10 ring-1 ring-cyan-500/20 hover:bg-cyan-500/20 text-cyan-700 dark:text-cyan-300",
  opencode:
    "bg-indigo-500/10 ring-1 ring-indigo-500/20 hover:bg-indigo-500/20 text-indigo-600 dark:text-indigo-400",
  openclaw:
    "bg-rose-500/10 ring-1 ring-rose-500/20 hover:bg-rose-500/20 text-rose-600 dark:text-rose-400",
  hermes:
    "bg-violet-500/10 ring-1 ring-violet-500/20 hover:bg-violet-500/20 text-violet-600 dark:text-violet-400",
  pi: "bg-fuchsia-500/10 ring-1 ring-fuchsia-500/20 hover:bg-fuchsia-500/20 text-fuchsia-600 dark:text-fuchsia-400",
};

const APP_BADGE_CLASSES: Record<string, string> = {
  claude: "bg-orange-500/10 text-orange-700 dark:text-orange-300",
  "claude-desktop": "bg-amber-500/10 text-amber-700 dark:text-amber-300",
  codex: "bg-green-500/10 text-green-700 dark:text-green-300",
  gemini: "bg-blue-500/10 text-blue-700 dark:text-blue-300",
  grokbuild: "bg-cyan-500/10 text-cyan-700 dark:text-cyan-300",
  opencode: "bg-indigo-500/10 text-indigo-700 dark:text-indigo-300",
  openclaw: "bg-rose-500/10 text-rose-700 dark:text-rose-300",
  hermes: "bg-violet-500/10 text-violet-700 dark:text-violet-300",
  pi: "bg-fuchsia-500/10 text-fuchsia-700 dark:text-fuchsia-300",
};

export function appActiveClass(appId: string): string {
  return (
    APP_ACTIVE_CLASSES[appId] ?? "bg-muted text-foreground ring-1 ring-border"
  );
}

export function appBadgeClass(appId: string): string {
  return APP_BADGE_CLASSES[appId] ?? "bg-muted text-muted-foreground";
}
