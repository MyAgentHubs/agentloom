import type { MemberUnit } from "../types/agent";
import { useI18n, type TranslationKey } from "../i18n";
import { relativeTime } from "../lib/relativeTime";

type TstateClass = "run" | "wait" | "done" | "fail";

function toTstate(status: MemberUnit["status"]): TstateClass {
  if (status === "running") return "run";
  if (status === "needs_input") return "wait";
  if (status === "done") return "done";
  return "fail";
}

type Props = {
  workers: MemberUnit[];
  onSelect: (assignmentId: string) => void;
  onStop: (assignmentId: string) => void;
};

export function TaskList({ workers, onSelect, onStop }: Props) {
  const { t } = useI18n();

  if (workers.length === 0) return null;
  return (
    <div className="tasklist">
      {workers.map((w) => {
        const ts = toTstate(w.status);
        const isRunning = w.status === "running";
        return (
          <div
            key={w.assignment_id}
            className="task-card"
            role="button"
            tabIndex={0}
            onClick={() => onSelect(w.assignment_id)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onSelect(w.assignment_id);
              }
            }}
          >
            <span className={`tstate ${ts}`} />
            <span className="tcbody">
              <p className="tcnm">{w.name}</p>
              {w.sub && <p className="tcsub">{w.sub}</p>}
              {w.started_at != null && (
                <p className="tcown">
                  {(() => {
                    const r = relativeTime(w.started_at, Date.now());
                    return t(r.key as TranslationKey, { n: r.n });
                  })()}
                </p>
              )}
            </span>
            {isRunning ? (
              <button
                className="tc-stop"
                aria-label={t("tasklist.stop")}
                title={t("tasklist.stop")}
                onClick={(e) => {
                  e.stopPropagation();
                  onStop(w.assignment_id);
                }}
                onKeyDown={(e) => e.stopPropagation()}
              >
                <svg viewBox="0 0 24 24">
                  <rect
                    x="6"
                    y="6"
                    width="12"
                    height="12"
                    rx="1.5"
                    fill="currentColor"
                    stroke="none"
                  />
                </svg>
              </button>
            ) : (
              <span />
            )}
          </div>
        );
      })}
    </div>
  );
}
