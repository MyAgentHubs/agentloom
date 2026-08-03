import type { GhGate } from "../types/repoManage";

export function computeGate(
  gitInstalled: boolean | null,
  ghInstalled: boolean | null,
  accountCount: number,
  install: { canBrew: boolean; installing: boolean; error?: string },
  accountError?: string,
): GhGate {
  if (gitInstalled === null || ghInstalled === null) {
    return { kind: "checking" };
  }
  if (!gitInstalled) return { kind: "missingGit" };
  if (!ghInstalled) {
    return {
      kind: "missing",
      canBrewInstall: install.canBrew,
      installing: install.installing,
      installError: install.error,
    };
  }
  if (accountError) return { kind: "accountError", message: accountError };
  if (accountCount === 0) return { kind: "noAccount" };
  return { kind: "ready" };
}
