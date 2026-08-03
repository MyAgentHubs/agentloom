// ⚠️ RemoteRepo 跨 IPC，字段必须 snake_case 镜像 Rust github::RemoteRepo（codebase 约定，
//    同 RepoMeta/NamespaceMeta；Rust 端默认 snake_case 序列化，无 camelCase rename）。
export type RemoteRepo = {
  owner: string;
  name: string;
  name_with_owner: string;
  is_private: boolean;
  is_empty: boolean;
  updated_at: string;
  // Rust Option<String> → serde None 序列化为 null（键存在）；TS 用 `| null` 不用 `?:`（codebase 约定，对齐 RepoMeta.owner: string | null）。
  description: string | null;
  language: string | null;
  language_color: string | null;
  cloned: boolean;
  repo_id: string | null;
  local_path: string | null;
};
export type RepoOpenSessionTarget = Pick<RemoteRepo, "repo_id" | "local_path">;
export type RepoKey = string; // 规范 = `github.com/${owner}/${name}`
export const repoKey = (r: { owner: string; name: string }): RepoKey =>
  `github.com/${r.owner}/${r.name}`;
// 内部专用类型（不跨 IPC）保持 camelCase。

export type RepoCacheEntry = {
  repos?: RemoteRepo[];
  updatedAt?: number;
  status: "idle" | "loading" | "refreshing" | "ready" | "error";
  error?: string;
  requestId: number;
  mutationGen: number;
};
export type RepoListView =
  | { kind: "idle" }
  | { kind: "cold-loading" }
  | { kind: "cold-error"; message: string }
  | {
      kind: "data";
      repos: RemoteRepo[];
      refreshing: boolean;
      refreshError?: string;
    };
export type CloneProgressEntry = {
  login: string;
  owner: string;
  name: string;
  order: number;
  phase: "cloning" | "done" | "fail" | "occupied";
  repoId?: string;
  message?: string;
  settledAt?: number;
};

export type GhGate =
  | { kind: "checking" }
  | { kind: "missingGit" }
  | { kind: "accountError"; message: string }
  | {
      kind: "missing";
      canBrewInstall: boolean;
      installing: boolean;
      installError?: string;
    }
  | { kind: "noAccount" }
  | { kind: "ready" };
export type CloneRowState =
  | { phase: "cloning" }
  | { phase: "done"; repoId: string }
  | { phase: "fail"; message: string }
  | { phase: "occupied"; message: string };
export type ListState =
  | { kind: "loading" }
  | { kind: "offline" }
  | { kind: "error"; message: string }
  | { kind: "ready"; repos: RemoteRepo[] };
export type RepoFilter = "all" | "cloned" | "remote";

export type RepoManagePanelProps = {
  accounts: { login: string; active: boolean; count?: number }[];
  selectedLogin: string;
  onSelectAccount: (login: string) => void;
  onConnectAccount: () => void;
  onConnectLocal: () => void;
  connectError?: string | null;
  gate: GhGate;
  onInstallGh: () => void;
  onRefreshAccounts: () => void;
  listState: ListState;
  onRetryList: () => void;
  search: string;
  onSearchChange: (q: string) => void;
  filter: RepoFilter;
  onFilterChange: (f: RepoFilter) => void;
  selected: Set<RepoKey>;
  onToggleSelect: (key: RepoKey) => void;
  baseFolderLabel: string;
  onClone: () => void;
  cloneProgress: Record<RepoKey, CloneProgressEntry>;
  onRetry: (key: RepoKey) => void;
  onRetryFailed?: () => void;
  onOpenSession: (repo: RepoOpenSessionTarget) => void;
};
