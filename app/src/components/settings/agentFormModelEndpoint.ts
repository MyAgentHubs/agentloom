import { CUSTOM_MODEL_SENTINEL, PROVIDER_PRESETS } from "./agentFormPresets";

import type { AccessMode, AccessPoint, ProviderId } from "./agentFormPresets";

export function classifyModelsFetchError(raw: string): string {
  if (raw === "missing_key") return "missing_key";
  const match = raw.match(/HTTP (\d{3})/);
  if (match) {
    const code = Number(match[1]);
    if (code === 401 || code === 403) return "auth";
    if (code === 429) return "rate_limit";
    if (code === 404) return "not_found";
    return "other";
  }
  return "network";
}

/** parse failure→null (a bad endpoint does not crash); success→origin+pathname (lowercase host·remove trailing slash). Exact matching·no substring. */
export function normalizeEndpoint(url: string): string | null {
  const t = url.trim();
  if (!t) return null;
  try {
    const u = new URL(t);
    return `${u.protocol}//${u.host.toLowerCase()}${u.pathname.replace(/\/+$/, "")}`;
  } catch {
    return null;
  }
}

/**
 * The URL used to fetch the model list for an access point—the single source of truth.
 * When the endpoint matches the access point, prefer its explicitly configured modelsEndpoint (the access point's models path
 * may not equal endpoint + "/models"—the anthropic path for borrow is completely different);
 * otherwise, fall back to concatenating according to the OpenAI convention (the scenario where an endpoint is entered manually for a custom harness).
 */
export function resolveModelsEndpoint(
  endpoint: string,
  accessPoint?: Pick<AccessPoint, "endpoint" | "modelsEndpoint">,
): string {
  const norm = normalizeEndpoint(endpoint);
  if (
    norm !== null &&
    norm === normalizeEndpoint(accessPoint?.endpoint ?? "") &&
    accessPoint?.modelsEndpoint
  ) {
    return accessPoint.modelsEndpoint;
  }
  return `${endpoint.trim().replace(/\/+$/, "")}/models`;
}

export type InferResult = {
  providerId: ProviderId;
  accessPointId: string | null;
};

export function inferProviderAccessPoint(agent: {
  endpoint?: string | null;
  provider?: string;
  access?: string;
}): InferResult {
  const ep = (agent.endpoint ?? "").trim();
  if (ep) {
    const norm = normalizeEndpoint(ep);
    if (norm) {
      for (const p of PROVIDER_PRESETS) {
        if (p.id === "custom") continue;
        for (const ap of p.accessPoints) {
          if (normalizeEndpoint(ap.endpoint) === norm) {
            return { providerId: p.id, accessPointId: ap.id };
          }
        }
      }
    }
    if (agent.access === "harness") {
      const prov = (agent.provider ?? "").toLowerCase();
      if (prov === "deepseek") {
        return { providerId: "harness-deepseek", accessPointId: null };
      }
      if (prov === "glm") {
        return { providerId: "harness-glm", accessPointId: null };
      }
      if (prov === "kimi") {
        return { providerId: "harness-kimi", accessPointId: null };
      }
      if (prov === "gemini") {
        return { providerId: "harness-gemini", accessPointId: null };
      }
    }
    return { providerId: "custom", accessPointId: null }; // Non-empty non-match/invalid URL → custom
  }
  const prov = (agent.provider ?? "").toLowerCase();
  if (prov === "claude" || prov === "anthropic") {
    return { providerId: "claude", accessPointId: null };
  }
  if (prov === "codex" || prov === "openai") {
    return { providerId: "codex", accessPointId: null };
  }
  if (prov === "z.ai" || prov === "bigmodel" || prov === "zhipu") {
    return { providerId: "zhipu", accessPointId: "cn" };
  }
  if (prov === "kimi") return { providerId: "kimi", accessPointId: "cn" };
  if (prov === "deepseek") {
    return { providerId: "deepseek", accessPointId: "default" };
  }
  return { providerId: "custom", accessPointId: null };
}

export function deriveAccess(
  preset: ProviderId,
  existingAccess?: string,
): AccessMode {
  if (existingAccess === "native") return "native";
  if (existingAccess === "borrow") return "borrow";
  if (existingAccess === "harness") return "harness";
  return (
    PROVIDER_PRESETS.find((provider) => provider.id === preset)?.access ??
    "borrow"
  );
}

export function mergeModelOptions(
  staticKnown: string[],
  liveCached: string[],
  currentValue: string,
): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const m of [...staticKnown, ...liveCached]) {
    const t = m.trim();
    if (t && t !== CUSTOM_MODEL_SENTINEL && !seen.has(t)) {
      seen.add(t);
      out.push(t);
    }
  }
  const cur = currentValue.trim();
  if (cur && cur !== CUSTOM_MODEL_SENTINEL && !seen.has(cur)) out.push(cur);
  out.push(CUSTOM_MODEL_SENTINEL);
  return out;
}

/** Dropdown options and the unknown-id hint; a non-null `nativeLive` (the local CLI's own list) is authoritative over the static table. */
export function resolveModelChoices(args: {
  staticModels: string[];
  liveModels: string[];
  nativeLive: string[] | null;
  current: string;
}): { options: string[]; unknown: boolean } {
  const { staticModels, liveModels, nativeLive, current } = args;
  const known = nativeLive ?? [...staticModels, ...liveModels];
  return {
    options: mergeModelOptions(
      nativeLive ?? staticModels,
      nativeLive ? [] : liveModels,
      current,
    ),
    unknown: current.trim() !== "" && !known.includes(current),
  };
}

const CACHE_TTL_MS = 24 * 60 * 60 * 1000;
function cacheKey(preset: string, endpoint: string): string {
  return `agentloom:models:${preset}:${normalizeEndpoint(endpoint) ?? endpoint}`;
}

export function writeModelCache(
  preset: string,
  endpoint: string,
  models: string[],
): void {
  const normalizedEndpoint = normalizeEndpoint(endpoint) ?? endpoint;
  try {
    localStorage.setItem(
      cacheKey(preset, endpoint),
      JSON.stringify({
        models,
        endpoint: normalizedEndpoint,
        fetchedAt: Date.now(),
      }),
    );
  } catch {
    /* localStorage unavailable·silent */
  }
}

export function readModelCache(
  preset: string,
  endpoint: string,
): string[] | null {
  try {
    const raw = localStorage.getItem(cacheKey(preset, endpoint));
    if (!raw) return null;
    const normalizedEndpoint = normalizeEndpoint(endpoint) ?? endpoint;
    const p = JSON.parse(raw) as {
      models?: unknown;
      endpoint?: unknown;
      fetchedAt?: unknown;
    };
    if (
      !Array.isArray(p.models) ||
      p.endpoint !== normalizedEndpoint ||
      typeof p.fetchedAt !== "number" ||
      Date.now() - p.fetchedAt > CACHE_TTL_MS
    )
      return null;
    return p.models.filter((m): m is string => typeof m === "string");
  } catch {
    return null;
  }
}
