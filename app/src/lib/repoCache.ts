import type {
  CloneProgressEntry,
  RemoteRepo,
  RepoCacheEntry,
  RepoKey,
  RepoListView,
} from "../types/repoManage";
import { repoKey } from "../types/repoManage";

// 设置 > 仓库列表持久化缓存（重启不再冷加载）：只落 localStorage，不接 sqlite/rust。
// 带版本号，deserialize 绝不 throw：顶层形状不对 / 版本不符 / JSON 解析失败 → 返回空 map 兜底；
// 单个 login entry 的形状不对（含 repos 数组元素形状不对）→ 只丢弃该 entry，其余 entry 照常保留。
export const REPO_CACHE_STORAGE_KEY = "agentloom.repoCache.v1";
const REPO_CACHE_STORAGE_VERSION = 1;

type PersistedRepoCacheEntry = {
  repos: RemoteRepo[];
  updatedAt?: number;
};

type PersistedRepoCachePayload = {
  version: number;
  data: Record<string, PersistedRepoCacheEntry>;
};

/**
 * 只挑可序列化、跨重启仍有意义的字段（repos + updatedAt）。
 * status/error/requestId/mutationGen 是本次运行时的瞬态/race-guard 状态，不落盘——
 * hydrate 时统一重置为 ready/0/0，交给既有 stale 判断逻辑驱动后台刷新。
 */
export function serializeRepoCache(
  map: Record<string, RepoCacheEntry>,
): string {
  const data: Record<string, PersistedRepoCacheEntry> = {};

  for (const [login, entry] of Object.entries(map)) {
    if (!entry.repos) continue;
    data[login] = { repos: entry.repos, updatedAt: entry.updatedAt };
  }

  const payload: PersistedRepoCachePayload = {
    version: REPO_CACHE_STORAGE_VERSION,
    data,
  };
  return JSON.stringify(payload);
}

/**
 * `string | null` 字段的严格校验：null 合法，但键缺失（undefined）不合法——
 * Rust `Option<String>` 序列化时键一定存在（None → null），所以严格校验不会误伤正常数据。
 * 注意 `typeof null === "object"`，必须先判 null 再判 string。
 */
function isStringOrNull(value: unknown): value is string | null {
  if (value === null) return true;
  return typeof value === "string";
}

/**
 * 逐字段严格校验 RemoteRepo 形状（镜像 src/types/repoManage.ts 的 12 个字段）。
 * 任何字段缺失/类型不对都判为不合法——脏数据（如旧版本写入的不兼容形状）绝不能
 * 被 `as` 断言硬转下去，否则会在下游（如 pruneSelection）读取 undefined 字段时炸掉。
 */
function isRemoteRepo(value: unknown): value is RemoteRepo {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  const r = value as Record<string, unknown>;
  return (
    typeof r.owner === "string" &&
    typeof r.name === "string" &&
    typeof r.name_with_owner === "string" &&
    typeof r.is_private === "boolean" &&
    typeof r.is_empty === "boolean" &&
    typeof r.updated_at === "string" &&
    isStringOrNull(r.description) &&
    isStringOrNull(r.language) &&
    isStringOrNull(r.language_color) &&
    typeof r.cloned === "boolean" &&
    isStringOrNull(r.repo_id) &&
    isStringOrNull(r.local_path)
  );
}

export function deserializeRepoCache(
  raw: string | null | undefined,
): Record<string, RepoCacheEntry> {
  if (!raw) return {};

  try {
    const parsed: unknown = JSON.parse(raw);
    if (
      !parsed ||
      typeof parsed !== "object" ||
      Array.isArray(parsed) ||
      (parsed as { version?: unknown }).version !== REPO_CACHE_STORAGE_VERSION
    ) {
      return {};
    }

    const data = (parsed as { data?: unknown }).data;
    if (!data || typeof data !== "object" || Array.isArray(data)) {
      return {};
    }

    const result: Record<string, RepoCacheEntry> = {};
    for (const [login, value] of Object.entries(
      data as Record<string, unknown>,
    )) {
      if (!value || typeof value !== "object" || Array.isArray(value)) {
        continue;
      }
      const entry = value as { repos?: unknown; updatedAt?: unknown };
      if (!Array.isArray(entry.repos) || !entry.repos.every(isRemoteRepo)) {
        continue;
      }

      result[login] = {
        repos: entry.repos,
        updatedAt:
          typeof entry.updatedAt === "number" ? entry.updatedAt : undefined,
        status: "ready",
        requestId: 0,
        mutationGen: 0,
      };
    }
    return result;
  } catch {
    return {};
  }
}

export function deriveView(entry?: RepoCacheEntry): RepoListView {
  if (!entry) return { kind: "idle" };

  if (!entry.repos) {
    if (entry?.status === "error") {
      return { kind: "cold-error", message: entry.error ?? "" };
    }

    if (entry.status === "idle") return { kind: "idle" };
    return { kind: "cold-loading" };
  }

  if (entry.status === "refreshing") {
    return { kind: "data", repos: entry.repos, refreshing: true };
  }

  if (entry.status === "error") {
    return {
      kind: "data",
      repos: entry.repos,
      refreshing: false,
      refreshError: entry.error,
    };
  }

  return { kind: "data", repos: entry.repos, refreshing: false };
}

export function mergeRefresh(
  prev: RepoCacheEntry,
  incoming: RemoteRepo[],
  fetchStartGen: number,
  cloneProgress: Record<RepoKey, CloneProgressEntry>,
): RepoCacheEntry | null {
  if (prev.mutationGen > fetchStartGen) {
    return null;
  }

  const incomingByKey = new Map<RepoKey, RemoteRepo>(
    incoming.map((repo) => [repoKey(repo), repo]),
  );
  const usedKeys = new Set<RepoKey>();
  const mergedRepos: RemoteRepo[] = [];

  const mergeRepo = (
    incomingRepo: RemoteRepo,
    localRepo?: RemoteRepo,
  ): RemoteRepo => {
    const key = repoKey(incomingRepo);

    if (cloneProgress[key]?.phase !== "done") {
      return incomingRepo;
    }

    return {
      ...incomingRepo,
      cloned: true,
      repo_id:
        localRepo?.repo_id ??
        cloneProgress[key]?.repoId ??
        incomingRepo.repo_id,
      local_path: localRepo?.local_path ?? incomingRepo.local_path,
    };
  };

  for (const localRepo of prev.repos ?? []) {
    const key = repoKey(localRepo);
    const incomingRepo = incomingByKey.get(key);

    if (!incomingRepo) {
      continue;
    }

    mergedRepos.push(mergeRepo(incomingRepo, localRepo));
    usedKeys.add(key);
  }

  for (const incomingRepo of incoming) {
    const key = repoKey(incomingRepo);

    if (usedKeys.has(key)) {
      continue;
    }

    mergedRepos.push(mergeRepo(incomingRepo));
    usedKeys.add(key);
  }

  return {
    ...prev,
    repos: mergedRepos,
    updatedAt: Date.now(),
    status: "ready",
    error: undefined,
  };
}

export function pruneSelection(
  selected: Set<RepoKey>,
  repos: RemoteRepo[],
): Set<RepoKey> {
  const cloneableKeys = new Set(
    repos
      .filter((repo) => !repo.cloned && !repo.is_empty)
      .map((repo) => repoKey(repo)),
  );

  return new Set([...selected].filter((key) => cloneableKeys.has(key)));
}
