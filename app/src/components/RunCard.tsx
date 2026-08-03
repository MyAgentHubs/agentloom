import type { Block } from "../types/agent";
import { useI18n } from "../i18n";

type Props = {
  block: Extract<Block, { type: "run_card" }>;
  onView: () => void;
  /** Only completed runs receive this callback, so unfinished runs have no undo action. */
  onUndo?: () => void;
};

export function RunCard({ block, onView, onUndo }: Props) {
  const { t } = useI18n();
  const { files_changed, insertions, deletions, interrupted } = block;
  const hasLineChanges = (insertions || 0) !== 0 || (deletions || 0) !== 0;
  const state = block.state ?? "active";
  const result = block.undo_result;
  const partiallyUndone = state === "partially_undone";
  const issueCount =
    (result?.report.skipped.length ?? 0) + (result?.report.failed.length ?? 0);
  const stateText =
    state === "undone"
      ? t("runCard.state.undone")
      : partiallyUndone
        ? t("runCard.state.partial", {
            undone: block.undo_undone ?? 0,
            total: block.undo_total ?? 0,
          })
        : t("runCard.state.completed");
  const unselectedThisAttempt = result
    ? Math.max(0, result.total_entries - result.selected_entries.length)
    : 0;
  const resultMeta = result
    ? [
        t("runCard.result.restored", {
          count: result.report.restored.length,
        }),
        result.report.skipped.length > 0
          ? t("runCard.result.skipped", {
              count: result.report.skipped.length,
            })
          : null,
        result.report.failed.length > 0
          ? t("runCard.result.failed", {
              count: result.report.failed.length,
            })
          : null,
        unselectedThisAttempt > 0
          ? t("runCard.result.unselected", { count: unselectedThisAttempt })
          : null,
      ]
        .filter((part): part is string => part !== null)
        .join(" · ")
    : null;
  const partialNote =
    result && issueCount > 0
      ? result.report.skipped.length > 0 && result.report.failed.length > 0
        ? t("runCard.partialNote.both", {
            skipped: result.report.skipped.length,
            failed: result.report.failed.length,
          })
        : result.report.skipped.length > 0
          ? t("runCard.partialNote.skipped", {
              count: result.report.skipped.length,
            })
          : t("runCard.partialNote.failed", {
              count: result.report.failed.length,
            })
      : null;

  return (
    <article
      className={`runcard run-card runcard--${state}${
        partiallyUndone ? " runcard--partial" : ""
      }`}
      role="group"
      aria-label={t("runCard.changesAria")}
    >
      <div className="run-card-head">
        <span className="run-icon" aria-hidden="true">
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
          >
            {state !== "active" ? (
              <path d="M9 7H4V2M4.5 7.5A8 8 0 1 1 4 16" />
            ) : (
              <>
                <path d="M12 3l7 4v10l-7 4-7-4V7z" />
                <path d="M8.5 10.5h7M8.5 14h4" />
              </>
            )}
          </svg>
        </span>
        <div className="run-summary runcard__summary">
          <div className="run-title">
            {t("runCard.summary", { files: files_changed })}
            <span
              className={`state-pill${state !== "active" ? " undone" : ""}${partiallyUndone ? " partial" : ""}`}
            >
              {stateText}
            </span>
          </div>
          <div className="run-meta">
            {state === "active" ? (
              <>
                {hasLineChanges && (
                  <>
                    <span className="runcard__stat runcard__stat--add">
                      +{insertions}
                    </span>{" "}
                    <span className="runcard__stat runcard__stat--del">
                      −{deletions}
                    </span>
                  </>
                )}
                {interrupted && (
                  <span className="runcard__interrupted">
                    {t("runCard.interrupted")}
                  </span>
                )}
              </>
            ) : result ? (
              resultMeta
            ) : null}
          </div>
        </div>
      </div>
      {partiallyUndone && partialNote && (
        <div className="run-note warn">{partialNote}</div>
      )}
      <div className="run-actions">
        {state !== "undone" && (
          <button
            type="button"
            className="run-action runcard__view"
            onClick={onView}
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.9"
              aria-hidden="true"
            >
              <path d="M3 12s3.5-6 9-6 9 6 9 6-3.5 6-9 6-9-6-9-6z" />
              <circle cx="12" cy="12" r="2.5" />
            </svg>
            {t("runCard.view")}
          </button>
        )}
        {(state === "active" || partiallyUndone) &&
          onUndo &&
          (block.undo_total ?? 0) > 0 && (
            <button
              type="button"
              className="run-action undo runcard__undo"
              onClick={onUndo}
            >
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.9"
                aria-hidden="true"
              >
                <path d="M9 7H4V2M4.5 7.5A8 8 0 1 1 4 16" />
              </svg>
              {result
                ? t("runCard.viewResult")
                : partiallyUndone
                  ? t("runCard.continueUndo")
                  : t("runCard.undo")}
            </button>
          )}
        {state === "undone" && result && onUndo && (
          <button
            type="button"
            className="run-action runcard__view"
            onClick={onUndo}
          >
            {t("runCard.viewResult")}
          </button>
        )}
        {state === "undone" && !result && (
          <button
            type="button"
            className="run-action runcard__view"
            onClick={onView}
          >
            {t("runCard.view")}
          </button>
        )}
      </div>
    </article>
  );
}
