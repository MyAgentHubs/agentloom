import { groupRepos } from "../lib/repoGroups";
import { useI18n } from "../i18n";
import type {
  CloneProgressEntry,
  RemoteRepo,
  RepoFilter,
  RepoKey,
  RepoOpenSessionTarget,
} from "../types/repoManage";
import { repoKey } from "../types/repoManage";

type Props = {
  repos: RemoteRepo[];
  selectedLogin: string;
  search: string;
  onSearchChange: (q: string) => void;
  filter: RepoFilter;
  onFilterChange: (f: RepoFilter) => void;
  selected: Set<RepoKey>;
  onToggleSelect: (key: RepoKey) => void;
  cloneProgress?: Record<RepoKey, CloneProgressEntry>;
  onRetry?: (key: RepoKey) => void;
  onOpenSession: (repo: RepoOpenSessionTarget) => void;
};

export function RepoList(p: Props) {
  const { t } = useI18n();
  const q = p.search.trim().toLowerCase();
  const matchRepoSearch = (r: RemoteRepo) =>
    !q ||
    r.name.toLowerCase().includes(q) ||
    r.name_with_owner.toLowerCase().includes(q);
  const matchRepoFilter = (r: RemoteRepo) =>
    p.filter === "all" ? true : p.filter === "cloned" ? r.cloned : !r.cloned;
  const visible = p.repos.filter(matchRepoSearch).filter(matchRepoFilter);
  const reposByKey = new Map<RepoKey, RemoteRepo>(
    p.repos.map((repo) => [repoKey(repo), repo]),
  );
  const visibleCloneProgress: Record<RepoKey, CloneProgressEntry> = {};
  for (const [key, entry] of Object.entries(p.cloneProgress ?? {})) {
    const repo = reposByKey.get(repoKey(entry));
    const nameWithOwner =
      repo?.name_with_owner ?? `${entry.owner}/${entry.name}`;
    const matchesSearch =
      !q ||
      entry.name.toLowerCase().includes(q) ||
      nameWithOwner.toLowerCase().includes(q);
    const batchIsCloned = entry.phase === "done" || repo?.cloned === true;
    const matchesFilter =
      p.filter === "all"
        ? true
        : p.filter === "cloned"
          ? batchIsCloned
          : !batchIsCloned;

    if (matchesSearch && matchesFilter) {
      visibleCloneProgress[key] = entry;
    }
  }
  const {
    batch,
    cloned: clonedRepos,
    remote: remoteRepos,
  } = groupRepos(visible, visibleCloneProgress, p.selectedLogin);

  return (
    <>
      <div className="rm-fixed">
        <div className="ob-search">
          <svg viewBox="0 0 24 24">
            <circle cx="11" cy="11" r="7" />
            <path d="M21 21l-4.3-4.3" />
          </svg>
          <input
            className="ph"
            placeholder={t("repoList.search.placeholder")}
            value={p.search}
            onChange={(e) => p.onSearchChange(e.target.value)}
          />
          {(["all", "cloned", "remote"] as RepoFilter[]).map((f) => (
            <span
              key={f}
              className={`filt${p.filter === f ? " on" : ""}`}
              onClick={() => p.onFilterChange(f)}
            >
              {f === "all" ? (
                t("repoList.filter.all")
              ) : f === "cloned" ? (
                <>
                  <span>{t("repoList.filter.cloned.first")}</span>
                  <span>{t("repoList.filter.cloned.second")}</span>
                </>
              ) : (
                <>
                  <span>{t("repoList.filter.remote.first")}</span>
                  <span>{t("repoList.filter.remote.second")}</span>
                </>
              )}
            </span>
          ))}
        </div>
      </div>

      <div className="rm-list">
        {batch.length > 0 && (
          <div className="ob-grp">{t("repoList.group.batch")}</div>
        )}
        {batch.map(({ entry, repo }) => {
          const key = repoKey(entry);
          const repoId = entry.repoId;
          const openTarget: RepoOpenSessionTarget | null =
            repo ?? (repoId ? { repo_id: repoId, local_path: null } : null);
          return (
            <div
              className={`ob-repo batch${entry.phase === "done" ? " cloned" : entry.phase === "occupied" ? " occupied" : ""}`}
              key={key}
            >
              <span
                className="rico"
                style={
                  entry.phase === "fail"
                    ? { background: "#f0ddd9" }
                    : entry.phase === "occupied"
                      ? { background: "var(--bg-sunken)" }
                      : undefined
                }
              >
                {entry.phase === "cloning" && (
                  <span
                    aria-label={t("repoList.status.cloningAria")}
                    style={{
                      width: 14,
                      height: 14,
                      borderRadius: "50%",
                      border: "2px solid var(--accent-soft)",
                      borderTopColor: "var(--accent)",
                      animation: "ob-spin 0.8s linear infinite",
                    }}
                  />
                )}
                {entry.phase === "done" && (
                  <svg
                    viewBox="0 0 24 24"
                    strokeWidth="3"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <path d="M20 6L9 17l-5-5" />
                  </svg>
                )}
                {entry.phase === "fail" && (
                  <svg
                    viewBox="0 0 24 24"
                    strokeWidth="3"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    style={{ stroke: "var(--red)" }}
                  >
                    <path d="M18 6L6 18M6 6l12 12" />
                  </svg>
                )}
                {entry.phase === "occupied" && (
                  <svg
                    viewBox="0 0 24 24"
                    strokeWidth="2.4"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    style={{ stroke: "var(--ink-3)" }}
                  >
                    <circle cx="12" cy="12" r="8" />
                    <path d="M12 8v5M12 16h.01" />
                  </svg>
                )}
              </span>
              <div className="body">
                <div className="nm">
                  {repo?.name ?? entry.name}
                  {repo?.is_private && <span className="priv">private</span>}
                  {repo?.is_empty && (
                    <span className="priv">{t("repoList.empty")}</span>
                  )}
                </div>
                <div className="meta">
                  {repo?.language && (
                    <span className="lang">
                      <span
                        className="d"
                        style={{ background: repo.language_color ?? undefined }}
                      />
                      {repo.language}
                    </span>
                  )}
                  {repo?.local_path ? (
                    <span className="path">{repo.local_path}</span>
                  ) : (
                    <span>
                      github.com/{entry.owner}/{entry.name}
                    </span>
                  )}
                </div>
              </div>
              <div
                className="right"
                style={{ display: "inline-flex", alignItems: "center", gap: 8 }}
              >
                {entry.phase === "cloning" && (
                  <span
                    style={{
                      color: "var(--accent)",
                      fontSize: 11,
                      fontWeight: 600,
                    }}
                  >
                    {t("repoList.status.cloning")}
                  </span>
                )}
                {entry.phase === "done" && openTarget && (
                  <span
                    className="ob-open"
                    onClick={() => p.onOpenSession(openTarget)}
                  >
                    {t("repoList.openSession")}
                  </span>
                )}
                {entry.phase === "fail" && (
                  <>
                    <span style={{ color: "var(--red)", fontSize: 11 }}>
                      {entry.message ?? t("repoList.status.cloneFailed")}
                    </span>
                    <span className="ob-retry" onClick={() => p.onRetry?.(key)}>
                      <svg
                        viewBox="0 0 24 24"
                        strokeWidth="2.4"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                      >
                        <path d="M21 12a9 9 0 11-2.6-6.3M21 4v5h-5" />
                      </svg>
                      {t("repoList.retry")}
                    </span>
                  </>
                )}
                {entry.phase === "occupied" && (
                  <span style={{ color: "var(--ink-3)", fontSize: 11 }}>
                    {t("repoList.status.occupied")}
                  </span>
                )}
              </div>
            </div>
          );
        })}

        {clonedRepos.length > 0 && (
          <div className="ob-grp">{t("repoList.group.cloned")}</div>
        )}
        {clonedRepos.map((r) => (
          <div className="ob-repo cloned" key={repoKey(r)}>
            <span className="rico">
              <svg
                viewBox="0 0 24 24"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M20 6L9 17l-5-5" />
              </svg>
            </span>
            <div className="body">
              <div className="nm">
                {r.name} {r.is_private && <span className="priv">private</span>}
              </div>
              <div className="meta">
                {r.language && (
                  <span className="lang">
                    <span
                      className="d"
                      style={{ background: r.language_color ?? undefined }}
                    />
                    {r.language}
                  </span>
                )}
                <span className="path">{r.local_path}</span>
              </div>
            </div>
            <div className="right">
              <span className="ob-open" onClick={() => p.onOpenSession(r)}>
                {t("repoList.openSession")}
              </span>
            </div>
          </div>
        ))}

        {remoteRepos.length > 0 && (
          <div className="ob-grp">
            {t("repoList.group.remote")}
            <span>{t("repoList.group.remoteHint")}</span>
          </div>
        )}
        {remoteRepos.map((r) => {
          const k = repoKey(r);
          const on = p.selected.has(k);
          return (
            <div className={`ob-repo remote${on ? " sel" : ""}`} key={k}>
              <span className="rico">
                <svg
                  viewBox="0 0 24 24"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  <ellipse cx="12" cy="5" rx="9" ry="3" />
                  <path d="M3 5v14c0 1.7 4 3 9 3s9-1.3 9-3V5" />
                  <path d="M3 12c0 1.7 4 3 9 3s9-1.3 9-3" />
                </svg>
              </span>
              <div className="body">
                <div className="nm">
                  {r.name}{" "}
                  {r.is_private && <span className="priv">private</span>}
                  {r.is_empty && (
                    <span className="priv">{t("repoList.empty")}</span>
                  )}
                </div>
                <div className="meta">
                  {r.language && (
                    <span className="lang">
                      <span
                        className="d"
                        style={{ background: r.language_color ?? undefined }}
                      />
                      {r.language}
                    </span>
                  )}
                  <span>
                    {t("repoList.updatedAt", { value: r.updated_at })}
                  </span>
                </div>
              </div>
              <div className="right">
                <span
                  className={`ob-cb${on ? " on" : ""}`}
                  aria-disabled={r.is_empty}
                  onClick={() => {
                    if (!r.is_empty) p.onToggleSelect(k);
                  }}
                >
                  <svg
                    viewBox="0 0 24 24"
                    strokeWidth="3"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <path d="M20 6L9 17l-5-5" />
                  </svg>
                </span>
              </div>
            </div>
          );
        })}
      </div>
    </>
  );
}
