import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  __resetSessionLifecycleForTests,
  DEFAULT_SESSION_LIFECYCLE_POLICY,
  getSessionLifecyclePolicy,
  latestActivityAt,
  normalizeLifecycleDays,
  normalizeSessionLifecyclePolicy,
  runSessionLifecycleSweep,
  SESSION_LIFECYCLE_PURGE_CUTOFF_STORAGE_KEY,
  SESSION_LIFECYCLE_STORAGE_KEY,
  setSessionLifecyclePolicy,
  shouldArchiveSession,
  shouldPurgeArchivedSession,
} from "./sessionLifecycle";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const invokeMock = vi.mocked(invoke);
const DAY = 24 * 60 * 60;

function enableLifecycle(nowSeconds: number) {
  setSessionLifecyclePolicy(
    {
      enabled: true,
      archiveAfterDays: 3,
      deleteArchivedAfterDays: 60,
    },
    nowSeconds,
  );
}

describe("session lifecycle policy", () => {
  beforeEach(() => {
    localStorage.clear();
    invokeMock.mockReset();
    __resetSessionLifecycleForTests();
  });

  it("defaults automation off with 3/60 thresholds", () => {
    expect(getSessionLifecyclePolicy()).toEqual({
      enabled: false,
      archiveAfterDays: 3,
      deleteArchivedAfterDays: 60,
    });
  });

  it("accepts one day and clamps invalid lower values", () => {
    expect(normalizeLifecycleDays(1, 3)).toBe(1);
    expect(normalizeLifecycleDays(0, 3)).toBe(1);
    expect(normalizeLifecycleDays(-5, 3)).toBe(1);
    expect(normalizeLifecycleDays("bad", 3)).toBe(3);
  });

  it("falls back safely for malformed persisted values", () => {
    localStorage.setItem(SESSION_LIFECYCLE_STORAGE_KEY, "not-json");
    __resetSessionLifecycleForTests();
    expect(getSessionLifecyclePolicy()).toEqual(
      DEFAULT_SESSION_LIFECYCLE_POLICY,
    );
  });

  it("persists normalized values across reload", () => {
    setSessionLifecyclePolicy({
      enabled: true,
      archiveAfterDays: 7,
      deleteArchivedAfterDays: 90,
    });
    __resetSessionLifecycleForTests();
    expect(getSessionLifecyclePolicy()).toEqual({
      enabled: true,
      archiveAfterDays: 7,
      deleteArchivedAfterDays: 90,
    });
  });

  it("normalizes old partial policies to disabled", () => {
    expect(normalizeSessionLifecyclePolicy({ archiveAfterDays: 4 })).toEqual({
      enabled: false,
      archiveAfterDays: 4,
      deleteArchivedAfterDays: 60,
    });
  });

  it("records the automatic purge cutoff only on first enable", () => {
    enableLifecycle(10 * DAY);
    expect(
      localStorage.getItem(SESSION_LIFECYCLE_PURGE_CUTOFF_STORAGE_KEY),
    ).toBe(String(10 * DAY));

    setSessionLifecyclePolicy(
      {
        enabled: false,
        archiveAfterDays: 3,
        deleteArchivedAfterDays: 60,
      },
      20 * DAY,
    );
    enableLifecycle(30 * DAY);

    expect(
      localStorage.getItem(SESSION_LIFECYCLE_PURGE_CUTOFF_STORAGE_KEY),
    ).toBe(String(10 * DAY));
  });
});

describe("session lifecycle boundaries", () => {
  const now = 100 * DAY;

  it("archives at the inactivity threshold, not before", () => {
    expect(shouldArchiveSession(now, now - 3 * DAY + 1, 3)).toBe(false);
    expect(shouldArchiveSession(now, now - 3 * DAY, 3)).toBe(true);
    expect(shouldArchiveSession(now, now - 4 * DAY, 3)).toBe(true);
  });

  it("uses recent message activity instead of old session creation time", () => {
    const createdAt = now - 30 * DAY;
    const messages = [
      { created_at: now - 10 * DAY },
      { created_at: now - DAY },
    ];
    const activity = latestActivityAt(createdAt, messages);
    expect(activity).toBe(now - DAY);
    expect(shouldArchiveSession(now, activity, 3)).toBe(false);
  });

  it("falls back to session creation when there are no timestamped messages", () => {
    expect(latestActivityAt(now - 4 * DAY, [{}, {}])).toBe(now - 4 * DAY);
  });

  it("purges at the archived retention threshold, not before", () => {
    expect(shouldPurgeArchivedSession(now, now - 60 * DAY + 1, 60)).toBe(false);
    expect(shouldPurgeArchivedSession(now, now - 60 * DAY, 60)).toBe(true);
    expect(shouldPurgeArchivedSession(now, now - 61 * DAY, 60)).toBe(true);
  });

  it("never retention-purges without archived_at", () => {
    expect(shouldPurgeArchivedSession(now, null, 60)).toBe(false);
  });
});

describe("session lifecycle sweep", () => {
  const now = 100 * DAY;

  beforeEach(() => {
    localStorage.clear();
    invokeMock.mockReset();
    __resetSessionLifecycleForTests();
  });

  it("does no maintenance while the master switch is off", async () => {
    await runSessionLifecycleSweep(now);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("archives a stale active session after opt-in", async () => {
    enableLifecycle(now);

    invokeMock.mockImplementation(async (command) => {
      if (command === "list_sessions") {
        return [
          {
            id: "stale",
            created_at: now - 10 * DAY,
            archived: false,
            archived_at: null,
          },
        ];
      }
      if (command === "get_messages") {
        return [{ created_at: now - 4 * DAY }];
      }
      return undefined;
    });

    await runSessionLifecycleSweep(now);

    expect(invokeMock).toHaveBeenCalledWith("set_session_archived", {
      id: "stale",
      archived: true,
    });
  });

  it("does not archive an old session with recent activity", async () => {
    enableLifecycle(now);

    invokeMock.mockImplementation(async (command) => {
      if (command === "list_sessions") {
        return [
          {
            id: "recent",
            created_at: now - 30 * DAY,
            archived: false,
            archived_at: null,
          },
        ];
      }
      if (command === "get_messages") {
        return [{ created_at: now - DAY }];
      }
      return undefined;
    });

    await runSessionLifecycleSweep(now);

    expect(invokeMock).not.toHaveBeenCalledWith(
      "set_session_archived",
      expect.anything(),
    );
  });

  it("protects pre-enable archives and purges only later eligible archives", async () => {
    enableLifecycle(20 * DAY);

    invokeMock.mockImplementation(async (command) => {
      if (command === "list_sessions") {
        return [
          {
            id: "legacy",
            created_at: 0,
            archived: true,
            archived_at: 10 * DAY,
          },
          {
            id: "eligible",
            created_at: 0,
            archived: true,
            archived_at: 40 * DAY,
          },
        ];
      }
      return undefined;
    });

    await runSessionLifecycleSweep(now);

    const calls = invokeMock.mock.calls.map(([command]) => command);
    expect(calls).toEqual(["list_sessions", "delete_session", "purge_session"]);
    expect(invokeMock).not.toHaveBeenCalledWith("delete_session", {
      id: "legacy",
    });
    expect(invokeMock).toHaveBeenCalledWith("delete_session", {
      id: "eligible",
    });
    expect(invokeMock).toHaveBeenCalledWith("purge_session", {
      id: "eligible",
    });
  });

  it("restores the tombstone when permanent purge fails", async () => {
    enableLifecycle(0);

    invokeMock.mockImplementation(async (command) => {
      if (command === "list_sessions") {
        return [
          {
            id: "retryable",
            created_at: now - 100 * DAY,
            archived: true,
            archived_at: now - 61 * DAY,
          },
        ];
      }
      if (command === "purge_session") throw new Error("gc failed");
      return undefined;
    });

    await runSessionLifecycleSweep(now);

    expect(invokeMock).toHaveBeenCalledWith("restore_session", {
      id: "retryable",
    });
  });

  it("coalesces concurrent sweeps into one single-flight run", async () => {
    enableLifecycle(now);

    let release: (() => void) | undefined;
    const blocked = new Promise<void>((resolve) => {
      release = resolve;
    });
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_sessions") {
        await blocked;
        return [];
      }
      return undefined;
    });

    const first = runSessionLifecycleSweep(now);
    const second = runSessionLifecycleSweep(now);
    release?.();
    await Promise.all([first, second]);

    expect(
      invokeMock.mock.calls.filter(([command]) => command === "list_sessions"),
    ).toHaveLength(1);
  });
});
