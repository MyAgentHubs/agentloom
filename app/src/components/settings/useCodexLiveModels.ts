import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { readModelCache, writeModelCache } from "./agentFormModelEndpoint";

function cachedSlugs(): string[] | null {
  const cached = readModelCache("codex", "");
  return cached && cached.length > 0 ? cached : null;
}

/**
 * Model ids reported by the local codex CLI, or null when unknown (disabled,
 * command failed, empty or malformed reply) so callers fall back to the
 * static preset list. A fresh 24h cache seeds the first render.
 */
export function useCodexLiveModels(enabled: boolean): string[] | null {
  const [models, setModels] = useState<string[] | null>(() =>
    enabled ? cachedSlugs() : null,
  );

  useEffect(() => {
    if (!enabled) {
      setModels(null);
      return;
    }
    setModels(cachedSlugs());
    let cancelled = false;
    invoke<unknown>("list_codex_models")
      .then((res) => {
        if (cancelled || !Array.isArray(res)) return;
        const slugs = [
          ...new Set(
            res.flatMap((m: { slug?: unknown } | null) =>
              typeof m?.slug === "string" && m.slug.trim() ? [m.slug] : [],
            ),
          ),
        ];
        if (slugs.length === 0) return;
        writeModelCache("codex", "", slugs);
        setModels(slugs);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [enabled]);

  return enabled ? models : null;
}
