import { describe, expect, it } from "vitest";
import type {
  CloneProgressEntry,
  RemoteRepo,
  RepoKey,
} from "../types/repoManage";
import { repoKey } from "../types/repoManage";
import { groupRepos } from "./repoGroups";

const remoteRepo = (
  owner: string,
  name: string,
  overrides: Partial<RemoteRepo> = {},
): RemoteRepo => ({
  owner,
  name,
  name_with_owner: `${owner}/${name}`,
  is_private: false,
  is_empty: false,
  updated_at: "2026-06-01T00:00:00Z",
  description: null,
  language: null,
  language_color: null,
  cloned: false,
  repo_id: null,
  local_path: null,
  ...overrides,
});

const progressEntry = (
  owner: string,
  name: string,
  order: number,
  overrides: Partial<CloneProgressEntry> = {},
): CloneProgressEntry => ({
  login: "octo",
  owner,
  name,
  order,
  phase: "cloning",
  ...overrides,
});

describe("groupRepos", () => {
  it("sorts cloned and remote repos by name while keeping batch rows in fixed order", () => {
    const alpha = remoteRepo("octo", "alpha");
    const beta = remoteRepo("octo", "beta");
    const clonedZulu = remoteRepo("octo", "zulu", { cloned: true });
    const clonedBravo = remoteRepo("octo", "Bravo", { cloned: true });
    const clonedAlpha = remoteRepo("octo", "amber", { cloned: true });
    const remoteZulu = remoteRepo("octo", "Zulu");
    const remoteBravo = remoteRepo("octo", "bravo");
    const remoteAlpha = remoteRepo("octo", "Acorn");
    const cloneProgress: Record<RepoKey, CloneProgressEntry> = {
      [repoKey(alpha)]: progressEntry("octo", "alpha", 2),
      [repoKey(beta)]: progressEntry("octo", "beta", 1),
      "github.com/work/gamma": progressEntry("work", "gamma", 0, {
        login: "work",
      }),
    };

    const groups = groupRepos(
      [
        alpha,
        beta,
        clonedZulu,
        remoteZulu,
        clonedBravo,
        remoteBravo,
        clonedAlpha,
        remoteAlpha,
      ],
      cloneProgress,
      "octo",
    );

    expect(groups.batch.map(({ entry }) => entry.name)).toEqual([
      "beta",
      "alpha",
    ]);
    expect(groups.cloned.map((repo) => repo.name)).toEqual([
      "amber",
      "Bravo",
      "zulu",
    ]);
    expect(groups.remote.map((repo) => repo.name)).toEqual([
      "Acorn",
      "bravo",
      "Zulu",
    ]);
  });

  it("keeps done batch keys out of cloned and remote groups until progress is cleared", () => {
    const done = remoteRepo("octo", "done", { cloned: true });
    const remote = remoteRepo("octo", "todo");
    const cloneProgress: Record<RepoKey, CloneProgressEntry> = {
      [repoKey(done)]: progressEntry("octo", "done", 0, {
        phase: "done",
        repoId: "repo-done",
      }),
    };

    const groups = groupRepos([done, remote], cloneProgress, "octo");

    expect(groups.batch.map(({ entry }) => entry.name)).toEqual(["done"]);
    expect(groups.cloned.map((repo) => repo.name)).toEqual([]);
    expect(groups.remote.map((repo) => repo.name)).toEqual(["todo"]);
  });

  it("falls back to entry identity when a batch repo is missing from the repo list", () => {
    const cloneProgress: Record<RepoKey, CloneProgressEntry> = {
      "github.com/octo/missing": progressEntry("octo", "missing", 0),
    };

    const groups = groupRepos([], cloneProgress, "octo");

    expect(groups.batch).toEqual([
      {
        entry: cloneProgress["github.com/octo/missing"],
        repo: undefined,
      },
    ]);
  });
});
