import type { CloneRowState, RepoKey } from "../types/repoManage";
import { useI18n } from "../i18n";

type Props = {
  destLabel: string;
  rows: Record<RepoKey, CloneRowState>;
  onRetry: (key: RepoKey) => void;
  onOpenSession: (repoId: string) => void;
};

export function CloneProgress({
  destLabel,
  rows,
  onRetry,
  onOpenSession,
}: Props) {
  const { t } = useI18n();
  const entries = Object.entries(rows);
  if (entries.length === 0) return null;
  return (
    <div className="ob-prog">
      <div className="dest">→ {destLabel}</div>
      {entries.map(([key, st]) => {
        const name = key.split("/").pop();
        return (
          <div
            className={`ob-prow${st.phase === "done" ? " done-row" : st.phase === "fail" ? " fail-row" : ""}`}
            key={key}
          >
            <span
              className={`st ${st.phase === "done" ? "done" : st.phase === "fail" ? "fail" : "spin"}`}
            >
              {st.phase === "done" && (
                <svg
                  viewBox="0 0 24 24"
                  strokeWidth="3"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  <path d="M20 6L9 17l-5-5" />
                </svg>
              )}
              {st.phase === "fail" && (
                <svg
                  viewBox="0 0 24 24"
                  strokeWidth="3"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  <path d="M18 6L6 18M6 6l12 12" />
                </svg>
              )}
            </span>
            <span className="nm">{name}</span>
            <span className="tail">
              {st.phase === "done" && (
                <span className="open" onClick={() => onOpenSession(st.repoId)}>
                  {t("cloneProgress.openSession")}
                </span>
              )}
              {st.phase === "cloning" && (
                <span className="pct">{t("cloneProgress.cloning")}</span>
              )}
              {st.phase === "occupied" && (
                <span className="pct">{t("cloneProgress.occupied")}</span>
              )}
              {st.phase === "fail" && (
                <>
                  <span style={{ color: "var(--red)" }}>{st.message}</span>
                  <span className="ob-retry" onClick={() => onRetry(key)}>
                    <svg
                      viewBox="0 0 24 24"
                      strokeWidth="2.4"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    >
                      <path d="M21 12a9 9 0 11-2.6-6.3M21 4v5h-5" />
                    </svg>
                    {t("cloneProgress.retry")}
                  </span>
                </>
              )}
            </span>
          </div>
        );
      })}
      <div className="nonblock">
        <svg
          viewBox="0 0 24 24"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M20 6L9 17l-5-5" />
        </svg>
        {t("cloneProgress.nonBlocking")}
      </div>
    </div>
  );
}
