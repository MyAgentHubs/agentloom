import { describe, expect, it } from "vitest";
import type {
  CloneProgressEntry,
  RemoteRepo,
  RepoCacheEntry,
  RepoKey,
} from "../types/repoManage";
import { repoKey } from "../types/repoManage";
import {
  deriveView,
  deserializeRepoCache,
  mergeRefresh,
  pruneSelection,
  serializeRepoCache,
} from "./repoCache";

const remoteRepo = (
  name: string,
  overrides: Partial<RemoteRepo> = {},
): RemoteRepo => ({
  owner: "octo",
  name,
  name_with_owner: `octo/${name}`,
  is_private: false,
  is_empty: false,
  updated_at: "2026-06-01T00:00:00Z",
  description: null,
  language: "TypeScript",
  language_color: "#3178c6",
  cloned: false,
  repo_id: null,
  local_path: null,
  ...overrides,
});

const cacheEntry = (
  overrides: Partial<RepoCacheEntry> = {},
): RepoCacheEntry => ({
  status: "idle",
  requestId: 7,
  mutationGen: 0,
  ...overrides,
});

describe("deriveView", () => {
  it("returns idle when there is no cache entry", () => {
    expect(deriveView()).toEqual({ kind: "idle" });
  });

  it("returns cold-loading only when a request is actually loading", () => {
    expect(deriveView(cacheEntry({ status: "loading" }))).toEqual({
      kind: "cold-loading",
    });
  });

  it("returns idle when an idle cache entry has no repos", () => {
    expect(deriveView(cacheEntry({ status: "idle" }))).toEqual({
      kind: "idle",
    });
  });

  it("returns cold-error when error has no repos", () => {
    expect(
      deriveView(cacheEntry({ status: "error", error: "NO_TOKEN:octo" })),
    ).toEqual({
      kind: "cold-error",
      message: "NO_TOKEN:octo",
    });
  });

  it("returns data with refreshing=true when repos are refreshing", () => {
    const repos = [remoteRepo("alpha")];

    expect(deriveView(cacheEntry({ repos, status: "refreshing" }))).toEqual({
      kind: "data",
      repos,
      refreshing: true,
    });
  });

  it("returns data with refreshError when repos exist and status is error", () => {
    const repos = [remoteRepo("alpha")];

    expect(
      deriveView(cacheEntry({ repos, status: "error", error: "offline" })),
    ).toEqual({
      kind: "data",
      repos,
      refreshing: false,
      refreshError: "offline",
    });
  });

  it("returns data with refreshing=false when repos are ready", () => {
    const repos = [remoteRepo("alpha")];

    expect(deriveView(cacheEntry({ repos, status: "ready" }))).toEqual({
      kind: "data",
      repos,
      refreshing: false,
    });
  });
});

describe("mergeRefresh", () => {
  it("returns null when a local mutation happened after fetch start", () => {
    const prev = cacheEntry({
      repos: [remoteRepo("alpha")],
      status: "refreshing",
      mutationGen: 3,
    });

    expect(mergeRefresh(prev, [remoteRepo("alpha")], 2, {})).toBeNull();
  });

  it("keeps local cloned fields for done clone progress keys", () => {
    const local = remoteRepo("alpha", {
      cloned: true,
      repo_id: "repo-local",
      local_path: "/Users/dev/code/alpha",
    });
    const incoming = remoteRepo("alpha", {
      cloned: false,
      repo_id: null,
      local_path: null,
    });
    const key = repoKey(local);
    const cloneProgress: Record<RepoKey, CloneProgressEntry> = {
      [key]: {
        login: "octo",
        owner: "octo",
        name: "alpha",
        order: 0,
        phase: "done",
        repoId: "repo-local",
      },
    };

    expect(
      mergeRefresh(
        cacheEntry({ repos: [local], status: "refreshing" }),
        [incoming],
        0,
        cloneProgress,
      ),
    ).toMatchObject({
      repos: [
        {
          name: "alpha",
          cloned: true,
          repo_id: "repo-local",
          local_path: "/Users/dev/code/alpha",
        },
      ],
      status: "ready",
    });
  });

  it("uses incoming values and keeps existing order before appending new repos", () => {
    const beta = remoteRepo("beta");
    const alpha = remoteRepo("alpha");
    const incomingAlpha = remoteRepo("alpha", {
      description: "fresh alpha",
      language: "Rust",
    });
    const incomingBeta = remoteRepo("beta", {
      description: "fresh beta",
      language: "Go",
    });
    const incomingGamma = remoteRepo("gamma", {
      description: "new gamma",
    });

    const merged = mergeRefresh(
      cacheEntry({
        repos: [beta, alpha],
        status: "refreshing",
        requestId: 11,
        mutationGen: 2,
      }),
      [incomingAlpha, incomingBeta, incomingGamma],
      2,
      {},
    );

    expect(merged?.repos?.map((repo) => repo.name)).toEqual([
      "beta",
      "alpha",
      "gamma",
    ]);
    expect(merged?.repos).toEqual([incomingBeta, incomingAlpha, incomingGamma]);
    expect(merged).toMatchObject({
      status: "ready",
      requestId: 11,
      mutationGen: 2,
    });
    expect(typeof merged?.updatedAt).toBe("number");
  });
});

describe("serializeRepoCache / deserializeRepoCache", () => {
  it("round-trips repos + updatedAt for entries that have data", () => {
    const repos = [remoteRepo("alpha"), remoteRepo("beta", { cloned: true })];
    const map: Record<string, RepoCacheEntry> = {
      octo: cacheEntry({ repos, updatedAt: 1700000000000, status: "ready" }),
    };

    const raw = serializeRepoCache(map);
    const restored = deserializeRepoCache(raw);

    expect(restored).toEqual({
      octo: {
        repos,
        updatedAt: 1700000000000,
        status: "ready",
        requestId: 0,
        mutationGen: 0,
      },
    });
  });

  it("drops entries without repos (no useful data to persist)", () => {
    const map: Record<string, RepoCacheEntry> = {
      octo: cacheEntry({ status: "loading" }),
      other: cacheEntry({ status: "error", error: "boom" }),
    };

    expect(deserializeRepoCache(serializeRepoCache(map))).toEqual({});
  });

  it("returns an empty map for null/undefined input", () => {
    expect(deserializeRepoCache(null)).toEqual({});
    expect(deserializeRepoCache(undefined)).toEqual({});
  });

  it("returns an empty map for corrupted JSON", () => {
    expect(deserializeRepoCache("{not valid json")).toEqual({});
  });

  it("returns an empty map when the version does not match", () => {
    const payload = JSON.stringify({
      version: 999,
      data: { octo: { repos: [remoteRepo("alpha")] } },
    });

    expect(deserializeRepoCache(payload)).toEqual({});
  });

  it("returns an empty map for non-object top-level shapes", () => {
    expect(deserializeRepoCache(JSON.stringify([1, 2, 3]))).toEqual({});
    expect(deserializeRepoCache(JSON.stringify("just a string"))).toEqual({});
    expect(deserializeRepoCache(JSON.stringify(42))).toEqual({});
  });

  it("returns an empty map when data is missing or malformed", () => {
    expect(deserializeRepoCache(JSON.stringify({ version: 1 }))).toEqual({});
    expect(
      deserializeRepoCache(JSON.stringify({ version: 1, data: [1, 2] })),
    ).toEqual({});
  });

  it("skips individual login entries whose shape is malformed but keeps valid ones", () => {
    const payload = JSON.stringify({
      version: 1,
      data: {
        good: { repos: [remoteRepo("alpha")], updatedAt: 123 },
        bad: { repos: "not-an-array" },
        alsoBad: null,
      },
    });

    expect(deserializeRepoCache(payload)).toEqual({
      good: {
        repos: [remoteRepo("alpha")],
        updatedAt: 123,
        status: "ready",
        requestId: 0,
        mutationGen: 0,
      },
    });
  });

  it("regression lock A: drops a login whose repos array contains null, and does not throw", () => {
    const payload = JSON.stringify({
      version: 1,
      data: {
        octo: { repos: [null], updatedAt: 123 },
      },
    });

    expect(() => deserializeRepoCache(payload)).not.toThrow();
    expect(deserializeRepoCache(payload)).toEqual({});
  });

  it("regression lock B: drops a login whose repos array contains non-object primitives", () => {
    const numberPayload = JSON.stringify({
      version: 1,
      data: { octo: { repos: [42] } },
    });
    const stringPayload = JSON.stringify({
      version: 1,
      data: { octo: { repos: ["oops"] } },
    });

    expect(deserializeRepoCache(numberPayload)).toEqual({});
    expect(deserializeRepoCache(stringPayload)).toEqual({});
  });

  it("drops a login whose repo element is missing required fields", () => {
    const payload = JSON.stringify({
      version: 1,
      data: {
        octo: { repos: [{ cloned: false, is_empty: false }] },
      },
    });

    expect(deserializeRepoCache(payload)).toEqual({});
  });

  it("drops a login whose repo element has a field with the wrong type", () => {
    const badRepo = { ...remoteRepo("a"), owner: 123 } as unknown;
    const payload = JSON.stringify({
      version: 1,
      data: {
        octo: { repos: [badRepo] },
      },
    });

    expect(deserializeRepoCache(payload)).toEqual({});
  });

  it("treats a missing string|null field key as invalid (undefined is not null)", () => {
    const badRepo = remoteRepo("a") as Partial<RemoteRepo>;
    delete badRepo.description;
    const payload = JSON.stringify({
      version: 1,
      data: {
        octo: { repos: [badRepo] },
      },
    });

    expect(deserializeRepoCache(payload)).toEqual({});
  });

  it("treats an explicit null for a string|null field as valid", () => {
    const repo = remoteRepo("a", { description: null });
    const payload = JSON.stringify({
      version: 1,
      data: {
        octo: { repos: [repo], updatedAt: 456 },
      },
    });

    expect(deserializeRepoCache(payload)).toEqual({
      octo: {
        repos: [repo],
        updatedAt: 456,
        status: "ready",
        requestId: 0,
        mutationGen: 0,
      },
    });
  });

  it("keeps a login with an empty repos array (no repos is a legitimate state)", () => {
    const payload = JSON.stringify({
      version: 1,
      data: {
        octo: { repos: [], updatedAt: 789 },
      },
    });

    expect(deserializeRepoCache(payload)).toEqual({
      octo: {
        repos: [],
        updatedAt: 789,
        status: "ready",
        requestId: 0,
        mutationGen: 0,
      },
    });
  });

  it("drops only the poisoned entry in a mixed payload, keeping the clean one (per-login failure granularity)", () => {
    const goodRepos = [remoteRepo("alpha")];
    const payload = JSON.stringify({
      version: 1,
      data: {
        good: { repos: goodRepos, updatedAt: 111 },
        poisoned: { repos: [remoteRepo("beta"), null], updatedAt: 222 },
      },
    });

    expect(deserializeRepoCache(payload)).toEqual({
      good: {
        repos: goodRepos,
        updatedAt: 111,
        status: "ready",
        requestId: 0,
        mutationGen: 0,
      },
    });
  });

  it("never throws for any malformed repo element shape", () => {
    const malformedPayloads = [
      JSON.stringify({ version: 1, data: { octo: { repos: [null] } } }),
      JSON.stringify({ version: 1, data: { octo: { repos: [42] } } }),
      JSON.stringify({ version: 1, data: { octo: { repos: ["oops"] } } }),
      JSON.stringify({
        version: 1,
        data: { octo: { repos: [{ cloned: false, is_empty: false }] } },
      }),
      JSON.stringify({
        version: 1,
        data: { octo: { repos: [{ ...remoteRepo("a"), owner: 123 }] } },
      }),
    ];

    for (const payload of malformedPayloads) {
      expect(() => deserializeRepoCache(payload)).not.toThrow();
    }
  });
});

describe("pruneSelection", () => {
  it("keeps only repos that still exist and are cloneable remotes", () => {
    const keep = remoteRepo("keep");
    const cloned = remoteRepo("cloned", { cloned: true });
    const empty = remoteRepo("empty", { is_empty: true });
    const missing = remoteRepo("missing");

    expect(
      pruneSelection(
        new Set([
          repoKey(keep),
          repoKey(cloned),
          repoKey(empty),
          repoKey(missing),
        ]),
        [keep, cloned, empty],
      ),
    ).toEqual(new Set([repoKey(keep)]));
  });
});
