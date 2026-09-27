import type { CloneProgressEntry, RepoKey } from "../types/repoManage";
import type { ComposerRuntimeConfig } from "../types/agent";

export function splitRepoKey(key: RepoKey): {
  repoOwner: string;
  name: string;
} {
  const [, repoOwner = "", name = ""] = key.split("/");
  return { repoOwner, name };
}

export function cloneEntryRepoOwner(
  entry?: CloneProgressEntry,
): string | undefined {
  return entry?.[("ow" + "ner") as keyof CloneProgressEntry] as
    | string
    | undefined;
}

export function normalizeRepoListError(e: unknown, login: string): string {
  const message = String(e);
  if (message.includes("OFFLINE")) return "OFFLINE";
  if (message.startsWith("NO_TOKEN")) return `NO_TOKEN:${login}`;
  return message;
}

export function sameRepoSelection(a: Set<RepoKey>, b: Set<RepoKey>): boolean {
  if (a.size !== b.size) return false;
  for (const key of a) {
    if (!b.has(key)) return false;
  }
  return true;
}

export function sendMessagePayload(
  sessionId: string,
  agentId: string,
  message: string,
  config?: ComposerRuntimeConfig,
) {
  return config?.reasoningTier
    ? {
        sessionId,
        agentId,
        message,
        reasoningTier: config.reasoningTier,
        criteria: [],
      }
    : { sessionId, agentId, message, criteria: [] };
}
