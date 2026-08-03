import type { CloneRowState, RepoKey } from "../types/repoManage";

const PATH_OCCUPIED = "PATH_OCCUPIED";

function cloneErrorState(e: unknown): CloneRowState {
  const message = String(e);
  if (message.includes(PATH_OCCUPIED)) {
    return { phase: "occupied", message };
  }
  return { phase: "fail", message };
}

export async function runClones(
  keys: RepoKey[],
  cloneOne: (key: RepoKey) => Promise<{ repoId: string }>,
  onUpdate: (key: RepoKey, st: CloneRowState) => void,
  concurrency = 4,
): Promise<void> {
  const workerCount = Math.max(1, Math.min(concurrency, keys.length));
  let nextIndex = 0;

  async function worker() {
    while (nextIndex < keys.length) {
      const key = keys[nextIndex];
      nextIndex += 1;
      onUpdate(key, { phase: "cloning" });
      try {
        const { repoId } = await cloneOne(key);
        onUpdate(key, { phase: "done", repoId });
      } catch (e) {
        onUpdate(key, cloneErrorState(e));
      }
    }
  }

  await Promise.all(Array.from({ length: workerCount }, () => worker()));
}
