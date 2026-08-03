import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";

export type TeamConfig = {
  /** null = no session-specific lead configured. */
  leadId: string | null;
  /** Explicit worker-pool member ids. Empty array is a valid saved Team config. */
  rosterIds: string[];
};

type SessionAgentConfigWire = {
  session_id?: string;
  sessionId?: string;
  lead_agent_id?: string | null;
  leadAgentId?: string | null;
  member_agent_ids?: string[];
  memberAgentIds?: string[];
};

const cache = new Map<string, TeamConfig>();
const committedCache = new Map<string, TeamConfig>();
const writeVersionBySession = new Map<string, number>();

type SessionWriteQueue = {
  desired: TeamConfig | null;
  inFlight: TeamConfig | null;
};

const writeQueuesBySession = new Map<string, SessionWriteQueue>();

function defaultConfig(): TeamConfig {
  return { leadId: null, rosterIds: [] };
}

function cloneConfig(config: TeamConfig): TeamConfig {
  return {
    leadId: config.leadId,
    rosterIds: [...config.rosterIds],
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function pickWireField(
  config: Record<string, unknown>,
  snakeKey: string,
  camelKey: string,
): unknown {
  if (Object.prototype.hasOwnProperty.call(config, snakeKey)) {
    return config[snakeKey];
  }
  return config[camelKey];
}

function normalizeMemberIds(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((id): id is string => typeof id === "string")
    : [];
}

function normalizeWireConfig(value: unknown): TeamConfig {
  const config = isRecord(value) ? value : {};
  const leadId = pickWireField(config, "lead_agent_id", "leadAgentId");
  const memberIds = pickWireField(config, "member_agent_ids", "memberAgentIds");

  return {
    leadId: typeof leadId === "string" ? leadId : null,
    rosterIds: normalizeMemberIds(memberIds),
  };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function configsEqual(a: TeamConfig, b: TeamConfig): boolean {
  return (
    a.leadId === b.leadId &&
    a.rosterIds.length === b.rosterIds.length &&
    a.rosterIds.every((id, index) => id === b.rosterIds[index])
  );
}

function currentWriteVersion(sessionId: string): number {
  return writeVersionBySession.get(sessionId) ?? 0;
}

function bumpWriteVersion(sessionId: string) {
  writeVersionBySession.set(sessionId, currentWriteVersion(sessionId) + 1);
}

function getCommittedTeamConfig(sessionId: string): TeamConfig {
  if (!sessionId) return defaultConfig();
  const committed = committedCache.get(sessionId);
  return committed ? cloneConfig(committed) : defaultConfig();
}

function commitConfig(sessionId: string, config: TeamConfig) {
  if (!sessionId) return false;

  committedCache.set(sessionId, cloneConfig(config));
  return true;
}

function restoreCommittedConfig(sessionId: string) {
  if (!sessionId) return;

  const committed = committedCache.get(sessionId);
  if (committed) {
    cache.set(sessionId, cloneConfig(committed));
  } else {
    cache.delete(sessionId);
  }
}

function hasPendingWrite(sessionId: string): boolean {
  const queue = writeQueuesBySession.get(sessionId);
  return Boolean(queue?.desired || queue?.inFlight);
}

function getWriteQueue(sessionId: string): SessionWriteQueue {
  const existing = writeQueuesBySession.get(sessionId);
  if (existing) return existing;

  const queue: SessionWriteQueue = { desired: null, inFlight: null };
  writeQueuesBySession.set(sessionId, queue);
  return queue;
}

function queueDesiredWrite(sessionId: string, config: TeamConfig) {
  const queue = getWriteQueue(sessionId);
  queue.desired = cloneConfig(config);
  bumpWriteVersion(sessionId);
  return queue;
}

function cleanupWriteQueue(sessionId: string) {
  const queue = writeQueuesBySession.get(sessionId);
  if (queue && !queue.desired && !queue.inFlight) {
    writeQueuesBySession.delete(sessionId);
  }
}

export function getCachedTeamConfig(sessionId: string): TeamConfig {
  if (!sessionId) return defaultConfig();
  const cached = cache.get(sessionId);
  return cached ? cloneConfig(cached) : defaultConfig();
}

export function load(sessionId: string): TeamConfig {
  return getCachedTeamConfig(sessionId);
}

export function clearTeamConfigCache() {
  cache.clear();
  committedCache.clear();
  writeVersionBySession.clear();
  writeQueuesBySession.clear();
}

export async function saveSessionTeamConfig(
  sessionId: string,
  config: TeamConfig,
): Promise<TeamConfig> {
  if (!sessionId) return defaultConfig();

  const writeConfig = cloneConfig(config);
  cache.set(sessionId, cloneConfig(writeConfig));
  try {
    const response = await invoke<SessionAgentConfigWire>(
      "set_session_agent_config",
      {
        sessionId,
        leadAgentId: writeConfig.leadId,
        memberAgentIds: writeConfig.rosterIds,
      },
    );
    const confirmed = normalizeWireConfig(response);
    cache.set(sessionId, cloneConfig(confirmed));
    commitConfig(sessionId, confirmed);
    return cloneConfig(confirmed);
  } catch (error) {
    restoreCommittedConfig(sessionId);
    throw error;
  }
}

function needsInitialRead(sessionId: string): boolean {
  return Boolean(sessionId && !committedCache.has(sessionId));
}

export function useTeamConfig(sessionId: string) {
  const [cfg, setCfg] = useState<TeamConfig>(() =>
    getCachedTeamConfig(sessionId),
  );
  const [loading, setLoading] = useState(() => needsInitialRead(sessionId));
  const [error, setError] = useState<string | null>(null);
  const cfgRef = useRef(cfg);
  const sessionIdRef = useRef(sessionId);
  const readSeqRef = useRef(0);
  const pendingReadRef = useRef<{ sessionId: string; seq: number } | null>(
    null,
  );

  sessionIdRef.current = sessionId;
  cfgRef.current = cfg;

  const applyConfig = useCallback(
    (
      targetSessionId: string,
      nextConfig: TeamConfig,
      options: { commit?: boolean } = {},
    ) => {
      const normalized = cloneConfig(nextConfig);
      if (targetSessionId) {
        cache.set(targetSessionId, cloneConfig(normalized));
        if (options.commit) {
          commitConfig(targetSessionId, normalized);
        }
      }
      if (sessionIdRef.current === targetSessionId) {
        cfgRef.current = normalized;
        setCfg(normalized);
      }
    },
    [],
  );

  const refresh = useCallback(async () => {
    const activeSessionId = sessionIdRef.current;
    const readSeq = ++readSeqRef.current;

    if (!activeSessionId) {
      pendingReadRef.current = null;
      applyConfig("", defaultConfig());
      setLoading(false);
      setError(null);
      return;
    }

    const writeVersionAtStart = currentWriteVersion(activeSessionId);
    const hadPendingWriteAtStart = hasPendingWrite(activeSessionId);
    pendingReadRef.current = { sessionId: activeSessionId, seq: readSeq };
    setLoading(true);
    setError(null);

    const canApplyRead = () =>
      readSeqRef.current === readSeq &&
      sessionIdRef.current === activeSessionId &&
      currentWriteVersion(activeSessionId) === writeVersionAtStart &&
      !hadPendingWriteAtStart &&
      !hasPendingWrite(activeSessionId);

    try {
      const response = await invoke<SessionAgentConfigWire>(
        "get_session_agent_config",
        { sessionId: activeSessionId },
      );
      if (canApplyRead()) {
        applyConfig(activeSessionId, normalizeWireConfig(response), {
          commit: true,
        });
      }
    } catch (err) {
      if (canApplyRead()) {
        setError(errorMessage(err));
      }
    } finally {
      if (
        pendingReadRef.current?.sessionId === activeSessionId &&
        pendingReadRef.current.seq === readSeq
      ) {
        pendingReadRef.current = null;
      }
      if (
        sessionIdRef.current === activeSessionId &&
        !hasPendingWrite(activeSessionId)
      ) {
        setLoading(false);
      }
    }
  }, [applyConfig]);

  useEffect(() => {
    const snapshot = getCachedTeamConfig(sessionId);
    cfgRef.current = snapshot;
    setCfg(snapshot);
    void refresh();
  }, [refresh, sessionId]);

  const flushWriteQueue = useCallback(
    async (targetSessionId: string) => {
      const initialQueue = writeQueuesBySession.get(targetSessionId);
      if (!initialQueue || initialQueue.inFlight) return;

      while (true) {
        const queue = writeQueuesBySession.get(targetSessionId);
        if (!queue?.desired) {
          cleanupWriteQueue(targetSessionId);
          break;
        }

        const writeConfig = cloneConfig(queue.desired);
        queue.desired = null;
        queue.inFlight = cloneConfig(writeConfig);

        try {
          const response = await invoke<SessionAgentConfigWire>(
            "set_session_agent_config",
            {
              sessionId: targetSessionId,
              leadAgentId: writeConfig.leadId,
              memberAgentIds: writeConfig.rosterIds,
            },
          );
          const confirmed = normalizeWireConfig(response);
          commitConfig(targetSessionId, confirmed);

          const latestDesired = queue.desired;
          if (!latestDesired || configsEqual(latestDesired, confirmed)) {
            queue.desired = null;
            applyConfig(targetSessionId, confirmed, { commit: true });
          }
        } catch (err) {
          if (!queue.desired) {
            applyConfig(
              targetSessionId,
              getCommittedTeamConfig(targetSessionId),
            );
            if (sessionIdRef.current === targetSessionId) {
              setError(errorMessage(err));
            }
          }
        } finally {
          queue.inFlight = null;
          cleanupWriteQueue(targetSessionId);
        }
      }

      if (
        sessionIdRef.current === targetSessionId &&
        pendingReadRef.current?.sessionId !== targetSessionId &&
        !hasPendingWrite(targetSessionId)
      ) {
        setLoading(false);
      }
    },
    [applyConfig],
  );

  const persistConfig = useCallback(
    (nextConfig: TeamConfig) => {
      const activeSessionId = sessionIdRef.current;

      if (!activeSessionId) {
        applyConfig("", defaultConfig());
        setLoading(false);
        setError(null);
        return;
      }

      const optimistic = cloneConfig(nextConfig);
      const queue = queueDesiredWrite(activeSessionId, optimistic);
      applyConfig(activeSessionId, optimistic);
      setLoading(true);
      setError(null);

      if (!queue.inFlight) {
        void flushWriteQueue(activeSessionId);
      }
    },
    [applyConfig, flushWriteQueue],
  );

  const setLeadId = useCallback(
    (id: string | null, defaultRosterIds?: string[]) => {
      const leadId = typeof id === "string" ? id : null;
      persistConfig({
        leadId,
        rosterIds:
          leadId === null ? [] : (defaultRosterIds ?? cfgRef.current.rosterIds),
      });
    },
    [persistConfig],
  );

  const setRosterIds = useCallback(
    (ids: string[] | null) => {
      persistConfig({
        leadId: cfgRef.current.leadId,
        rosterIds: normalizeMemberIds(ids),
      });
    },
    [persistConfig],
  );

  const toggleRoster = useCallback(
    (id: string, _allEnabledIds: string[]) => {
      const current = cfgRef.current;
      const rosterIds = current.rosterIds.includes(id)
        ? current.rosterIds.filter((existingId) => existingId !== id)
        : [...current.rosterIds, id];
      persistConfig({ leadId: current.leadId, rosterIds });
    },
    [persistConfig],
  );

  return {
    leadId: cfg.leadId,
    rosterIds: cfg.rosterIds,
    loading: loading || (error === null && needsInitialRead(sessionId)),
    error,
    refresh,
    setLeadId,
    setRosterIds,
    toggleRoster,
  };
}
