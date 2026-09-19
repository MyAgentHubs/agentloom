import { invoke } from "@tauri-apps/api/core";
import { useSyncExternalStore } from "react";

export type SessionLifecyclePolicy = {
  enabled: boolean;
  archiveAfterDays: number;
  deleteArchivedAfterDays: number;
};

export const SESSION_LIFECYCLE_STORAGE_KEY = "agentloom.sessionLifecycle.v1";
export const SESSION_LIFECYCLE_PURGE_CUTOFF_STORAGE_KEY =
  "agentloom.sessionLifecycle.purgeEligibleSince.v1";
export const DEFAULT_SESSION_LIFECYCLE_POLICY: SessionLifecyclePolicy = {
  enabled: false,
  archiveAfterDays: 3,
  deleteArchivedAfterDays: 60,
};
export const MIN_SESSION_LIFECYCLE_DAYS = 1;
export const SESSION_LIFECYCLE_SWEEP_INTERVAL_MS = 60 * 60 * 1000;
const DAY_SECONDS = 24 * 60 * 60;

type LifecycleSession = {
  id: string;
  created_at: number;
  archived: boolean;
  archived_at: number | null;
};

type LifecycleMessage = {
  created_at?: number;
};

export type SessionLifecycleCandidate = {
  id: string;
  action: "archive" | "purge";
};

function hasLocalStorage(): boolean {
  return typeof localStorage !== "undefined";
}

export function normalizeLifecycleDays(
  value: unknown,
  fallback: number,
): number {
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.max(MIN_SESSION_LIFECYCLE_DAYS, Math.floor(parsed));
}

export function normalizeSessionLifecyclePolicy(
  value: Partial<SessionLifecyclePolicy> | null | undefined,
): SessionLifecyclePolicy {
  return {
    enabled: value?.enabled === true,
    archiveAfterDays: normalizeLifecycleDays(
      value?.archiveAfterDays,
      DEFAULT_SESSION_LIFECYCLE_POLICY.archiveAfterDays,
    ),
    deleteArchivedAfterDays: normalizeLifecycleDays(
      value?.deleteArchivedAfterDays,
      DEFAULT_SESSION_LIFECYCLE_POLICY.deleteArchivedAfterDays,
    ),
  };
}

function readInitialPolicy(): SessionLifecyclePolicy {
  if (!hasLocalStorage()) return DEFAULT_SESSION_LIFECYCLE_POLICY;
  try {
    const raw = localStorage.getItem(SESSION_LIFECYCLE_STORAGE_KEY);
    if (!raw) return DEFAULT_SESSION_LIFECYCLE_POLICY;
    return normalizeSessionLifecyclePolicy(JSON.parse(raw));
  } catch {
    return DEFAULT_SESSION_LIFECYCLE_POLICY;
  }
}

function readInitialPurgeCutoff(): number | null {
  if (!hasLocalStorage()) return null;
  try {
    const raw = localStorage.getItem(
      SESSION_LIFECYCLE_PURGE_CUTOFF_STORAGE_KEY,
    );
    if (raw === null) return null;
    const parsed = Number(raw);
    if (!Number.isFinite(parsed) || parsed < 0) return null;
    return Math.floor(parsed);
  } catch {
    return null;
  }
}

let currentPolicy = readInitialPolicy();
let currentPurgeCutoff = readInitialPurgeCutoff();
const listeners = new Set<() => void>();

export function getSessionLifecyclePolicy(): SessionLifecyclePolicy {
  return currentPolicy;
}

function ensurePurgeCutoff(nowSeconds: number): number {
  if (currentPurgeCutoff !== null) return currentPurgeCutoff;

  currentPurgeCutoff = Math.max(0, Math.floor(nowSeconds));
  if (hasLocalStorage()) {
    try {
      localStorage.setItem(
        SESSION_LIFECYCLE_PURGE_CUTOFF_STORAGE_KEY,
        String(currentPurgeCutoff),
      );
    } catch {
      // Keep the in-memory protection even when persistence is unavailable.
    }
  }
  return currentPurgeCutoff;
}

export function setSessionLifecyclePolicy(
  next: SessionLifecyclePolicy,
  nowSeconds = Math.floor(Date.now() / 1000),
): void {
  const normalized = normalizeSessionLifecyclePolicy(next);

  // First opt-in establishes a permanent safety boundary: archives that already
  // existed before automation was enabled never become automatic purge targets.
  if (normalized.enabled && !currentPolicy.enabled) {
    ensurePurgeCutoff(nowSeconds);
  }

  currentPolicy = normalized;
  if (hasLocalStorage()) {
    try {
      localStorage.setItem(
        SESSION_LIFECYCLE_STORAGE_KEY,
        JSON.stringify(currentPolicy),
      );
    } catch {
      // Keep the setting usable when persistence is unavailable.
    }
  }
  for (const listener of listeners) listener();
}

function subscribeSessionLifecyclePolicy(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useSessionLifecyclePolicy(): [
  SessionLifecyclePolicy,
  (next: SessionLifecyclePolicy) => void,
] {
  const policy = useSyncExternalStore(
    subscribeSessionLifecyclePolicy,
    getSessionLifecyclePolicy,
    () => DEFAULT_SESSION_LIFECYCLE_POLICY,
  );
  return [policy, setSessionLifecyclePolicy];
}

export function latestActivityAt(
  sessionCreatedAt: number,
  messages: LifecycleMessage[],
): number {
  return messages.reduce(
    (latest, message) =>
      typeof message.created_at === "number"
        ? Math.max(latest, message.created_at)
        : latest,
    sessionCreatedAt,
  );
}

export function shouldArchiveSession(
  nowSeconds: number,
  lastActivityAt: number,
  archiveAfterDays: number,
): boolean {
  return nowSeconds - lastActivityAt >= archiveAfterDays * DAY_SECONDS;
}

export function shouldPurgeArchivedSession(
  nowSeconds: number,
  archivedAt: number | null,
  deleteArchivedAfterDays: number,
): boolean {
  return (
    archivedAt !== null &&
    nowSeconds - archivedAt >= deleteArchivedAfterDays * DAY_SECONDS
  );
}

let sweepInFlight: Promise<void> | null = null;

async function permanentlyDeleteArchivedSession(id: string): Promise<void> {
  await invoke("delete_session", { id });
  try {
    await invoke("purge_session", { id });
  } catch (error) {
    // delete_session is reversible. If permanent cleanup fails, put the session
    // back so a later sweep can retry instead of silently leaving it in Trash.
    try {
      await invoke("restore_session", { id });
    } catch (restoreError) {
      console.error(
        "session lifecycle purge failed and restore also failed",
        id,
        error,
        restoreError,
      );
    }
    throw error;
  }
}

async function runSessionLifecycleSweepOnce(nowSeconds: number): Promise<void> {
  const policy = getSessionLifecyclePolicy();
  if (!policy.enabled) return;

  // Fail safe if an enabled policy is restored without the migration marker:
  // establish the boundary before reading or mutating any sessions.
  const purgeCutoff = ensurePurgeCutoff(nowSeconds);
  const sessions = await invoke<LifecycleSession[]>("list_sessions");

  for (const session of sessions) {
    // Honor a user disabling automation while a sweep is already in progress.
    if (!getSessionLifecyclePolicy().enabled) return;

    if (session.archived) {
      const archiveIsEligibleForAutomaticPurge =
        session.archived_at !== null && session.archived_at >= purgeCutoff;

      if (
        archiveIsEligibleForAutomaticPurge &&
        shouldPurgeArchivedSession(
          nowSeconds,
          session.archived_at,
          policy.deleteArchivedAfterDays,
        )
      ) {
        try {
          await permanentlyDeleteArchivedSession(session.id);
        } catch (error) {
          console.error(
            "session lifecycle permanent purge failed",
            session.id,
            error,
          );
        }
      }
      continue;
    }

    // Avoid loading message history for sessions that cannot possibly be stale.
    if (
      !shouldArchiveSession(
        nowSeconds,
        session.created_at,
        policy.archiveAfterDays,
      )
    ) {
      continue;
    }

    try {
      const messages = await invoke<LifecycleMessage[]>("get_messages", {
        sessionId: session.id,
      });
      const lastActivity = latestActivityAt(session.created_at, messages);
      if (
        shouldArchiveSession(nowSeconds, lastActivity, policy.archiveAfterDays)
      ) {
        await invoke("set_session_archived", {
          id: session.id,
          archived: true,
        });
      }
    } catch (error) {
      // One busy/corrupt session must not stop maintenance for every other one.
      console.error("session lifecycle auto-archive failed", session.id, error);
    }
  }
}

export function runSessionLifecycleSweep(
  nowSeconds = Math.floor(Date.now() / 1000),
): Promise<void> {
  if (sweepInFlight) return sweepInFlight;
  sweepInFlight = runSessionLifecycleSweepOnce(nowSeconds).finally(() => {
    sweepInFlight = null;
  });
  return sweepInFlight;
}

export function installSessionLifecycleMaintenance(): () => void {
  let timer: number | null = null;

  const run = () => {
    void runSessionLifecycleSweep().catch((error) => {
      console.error("session lifecycle sweep failed", error);
    });
  };

  const stopTimer = () => {
    if (timer === null) return;
    window.clearInterval(timer);
    timer = null;
  };

  const syncEnabledState = () => {
    if (!getSessionLifecyclePolicy().enabled) {
      stopTimer();
      return;
    }
    if (timer !== null) return;

    run();
    timer = window.setInterval(run, SESSION_LIFECYCLE_SWEEP_INTERVAL_MS);
  };

  const unsubscribe = subscribeSessionLifecyclePolicy(syncEnabledState);
  syncEnabledState();

  return () => {
    unsubscribe();
    stopTimer();
  };
}

/** Test-only: reload module-level preference state from storage. */
export function __resetSessionLifecycleForTests(): void {
  currentPolicy = readInitialPolicy();
  currentPurgeCutoff = readInitialPurgeCutoff();
  sweepInFlight = null;
  listeners.clear();
}
