import type {
  CloneProgressEntry,
  RemoteRepo,
  RepoKey,
} from "../types/repoManage";
import { repoKey } from "../types/repoManage";

export type RepoGroups = {
  batch: { entry: CloneProgressEntry; repo?: RemoteRepo }[];
  cloned: RemoteRepo[];
  remote: RemoteRepo[];
};

export function groupRepos(
  repos: RemoteRepo[],
  cloneProgress: Record<RepoKey, CloneProgressEntry>,
  selectedLogin: string,
): RepoGroups {
  const reposByKey = new Map<RepoKey, RemoteRepo>(
    repos.map((repo) => [repoKey(repo), repo]),
  );
  const currentProgressEntries = Object.entries(cloneProgress).filter(
    ([, entry]) => entry.login === selectedLogin,
  );
  const currentProgressKeys = new Set(
    currentProgressEntries.map(([key]) => key),
  );

  const batch = currentProgressEntries
    .map(([, entry]) => ({
      entry,
      repo: reposByKey.get(repoKey(entry)),
    }))
    .sort((a, b) => a.entry.order - b.entry.order);

  const cloned = repos
    .filter((repo) => repo.cloned && !currentProgressKeys.has(repoKey(repo)))
    .sort((a, b) =>
      a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    );
  const remote = repos
    .filter((repo) => !repo.cloned && !currentProgressKeys.has(repoKey(repo)))
    .sort((a, b) =>
      a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    );

  return { batch, cloned, remote };
}
