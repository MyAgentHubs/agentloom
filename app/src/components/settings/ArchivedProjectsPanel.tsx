import { useCallback, useEffect, useState, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "../../i18n";
import { renderBackendError } from "../../lib/backendMsg";
import type { RepoMeta } from "../../types/agent";
import { ConfirmDialog } from "../ConfirmDialog";

type Props = {
  onArchivedChanged: () => void;
};

const styles = {
  list: {
    display: "flex",
    flexDirection: "column",
    gap: 7,
    listStyle: "none",
    margin: 0,
    maxWidth: 760,
    padding: 0,
  },
  row: {
    alignItems: "center",
    display: "flex",
    gap: 11,
    padding: "10px 12px",
  },
  body: {
    flex: 1,
    minWidth: 0,
  },
  name: {
    color: "var(--ink)",
    fontSize: 13,
    fontWeight: 600,
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  actions: {
    alignItems: "center",
    display: "flex",
    gap: 6,
    marginLeft: 8,
  },
  empty: {
    color: "var(--ink-3)",
    fontSize: 12,
    padding: "18px 0",
  },
  error: {
    color: "var(--red)",
    fontSize: 12,
    marginBottom: 10,
  },
} satisfies Record<string, CSSProperties>;

export function ArchivedProjectsPanel({ onArchivedChanged }: Props) {
  const { t } = useI18n();
  const [repos, setRepos] = useState<RepoMeta[]>([]);
  const [loading, setLoading] = useState(true);
  const [actionId, setActionId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [deleteForeverTarget, setDeleteForeverTarget] = useState<{
    id: string;
    name: string;
  } | null>(null);

  const loadArchived = useCallback(async () => {
    const archived = await invoke<RepoMeta[]>("list_repos_by_status", {
      status: "archived",
    });
    setRepos(archived);
  }, []);

  useEffect(() => {
    void loadArchived()
      .catch((cause) => setError(renderBackendError(String(cause), t)))
      .finally(() => setLoading(false));
  }, [loadArchived, t]);

  async function restore(repo: RepoMeta) {
    if (actionId) return;
    setActionId(repo.id);
    setError(null);
    try {
      await invoke("restore_repo", { id: repo.id });
      await loadArchived();
      onArchivedChanged();
    } catch (cause) {
      setError(renderBackendError(String(cause), t));
    } finally {
      setActionId(null);
    }
  }

  async function deleteForever() {
    if (!deleteForeverTarget || actionId) return;
    const { id } = deleteForeverTarget;
    setActionId(id);
    setError(null);
    try {
      await invoke("delete_repo_forever", { id });
      setDeleteForeverTarget(null);
      await loadArchived();
      onArchivedChanged();
    } catch (cause) {
      setError(renderBackendError(String(cause), t));
    } finally {
      setActionId(null);
    }
  }

  return (
    <>
      {error ? (
        <div role="alert" style={styles.error}>
          {error}
        </div>
      ) : null}
      {loading ? (
        <div className="ob-sk line w2" />
      ) : repos.length === 0 ? (
        <div style={styles.empty}>{t("archivedProjects.empty")}</div>
      ) : (
        <ul style={styles.list}>
          {repos.map((repo) => (
            <li
              key={repo.id}
              className="st-agent"
              data-testid={`archived-project-row-${repo.id}`}
              style={styles.row}
            >
              <span className="project-icon" aria-hidden="true">
                {repo.icon || "📁"}
              </span>
              <div style={styles.body}>
                <div style={styles.name}>{repo.name}</div>
              </div>
              <div style={styles.actions}>
                <button
                  type="button"
                  className="ob-btn"
                  disabled={actionId !== null}
                  onClick={() => void restore(repo)}
                >
                  {t("archivedProjects.restore")}
                </button>
                <button
                  type="button"
                  className="ob-btn"
                  disabled={actionId !== null}
                  onClick={() =>
                    setDeleteForeverTarget({ id: repo.id, name: repo.name })
                  }
                >
                  {t("archivedProjects.deleteForever")}
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
      <ConfirmDialog
        open={deleteForeverTarget !== null}
        title={t("archivedProjects.deleteConfirm.title")}
        body={t("archivedProjects.deleteConfirm.body", {
          name: deleteForeverTarget?.name ?? "",
        })}
        confirmLabel={t("archivedProjects.deleteConfirm.confirm")}
        cancelLabel={t("archivedProjects.deleteConfirm.cancel")}
        tone="danger"
        onConfirm={() => void deleteForever()}
        onCancel={() => {
          if (!actionId) setDeleteForeverTarget(null);
        }}
      />
    </>
  );
}
