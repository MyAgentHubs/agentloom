import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "../i18n";
import { buildUndoDiff, buildUndoRequest } from "../lib/undo";
import type { UndoDiffResult } from "../lib/undoDiffTypes";
import type {
  ChangeKind,
  UndoEntry,
  UndoIssue,
  UndoReport,
  UndoResultRecord,
} from "../types/undo";
import { UndoCheckbox } from "./UndoCheckbox";

type Props = {
  sessionId: string;
  runId: string;
  initialResult?: UndoResultRecord | null;
  onBack: () => void;
  onComplete: (result: UndoResultRecord) => void;
};

type Outcome = {
  filePath: string;
  changeKind: ChangeKind;
  kind: "restored" | "skipped" | "failed";
  reason?: string;
};

type SkipReasonKind =
  | "changed"
  | "unsafe"
  | "alreadyUndone"
  | "stale"
  | "unknown";

function skipReasonKind(reason: string | undefined): SkipReasonKind {
  if (reason?.includes("file changed after the undo list was viewed")) {
    return "changed";
  }
  if (reason?.includes("checkpoint path could not be safely resolved")) {
    return "unsafe";
  }
  if (reason?.includes("checkpoint entry was already undone")) {
    return "alreadyUndone";
  }
  // F1 纵深防御：后端 undo_run_edits_inner 在写回前再验一遍新鲜度——正常 UI 流程走不到
  // 这里（stale 条目在列表阶段就禁止勾选），这条分支兜的是绕过 UI 直接调 IPC 的情况。
  if (reason?.includes("checkpoint entry is stale")) {
    return "stale";
  }
  return "unknown";
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function previewProblem(
  entry: UndoEntry,
): "binary" | "tooLarge" | "unsupported" | null {
  if (
    entry.is_binary ||
    entry.preimage_preview.kind === "binary" ||
    entry.current_preview.kind === "binary"
  ) {
    return "binary";
  }
  if (
    entry.preimage_preview.kind === "too_large" ||
    entry.current_preview.kind === "too_large"
  ) {
    return "tooLarge";
  }
  if (
    entry.preimage_preview.kind === "unsupported" ||
    entry.current_preview.kind === "unsupported"
  ) {
    return "unsupported";
  }
  return null;
}

function changeKindClass(kind: ChangeKind): string {
  return kind === "created" ? "created" : kind === "deleted" ? "deleted" : "";
}

function outcomeForPath(
  report: UndoReport,
  filePath: string,
): Pick<Outcome, "kind" | "reason"> | null {
  if (report.restored.includes(filePath)) return { kind: "restored" };
  const skipped = report.skipped.find((item) => item.file_path === filePath);
  if (skipped) return { kind: "skipped", reason: skipped.reason };
  const failed = report.failed.find((item) => item.file_path === filePath);
  if (failed) return { kind: "failed", reason: failed.reason };
  return null;
}

function resultOutcomes(result: UndoResultRecord): Outcome[] {
  const outcomes: Outcome[] = [];
  const seen = new Set<string>();
  for (const selected of result.selected_entries) {
    const outcome = outcomeForPath(result.report, selected.file_path);
    if (!outcome) continue;
    seen.add(selected.file_path);
    outcomes.push({
      filePath: selected.file_path,
      changeKind: selected.change_kind,
      ...outcome,
    });
  }
  const addIssue = (kind: "skipped" | "failed", issue: UndoIssue) => {
    if (seen.has(issue.file_path)) return;
    seen.add(issue.file_path);
    outcomes.push({
      filePath: issue.file_path,
      changeKind: "modified",
      kind,
      reason: issue.reason,
    });
  };
  for (const filePath of result.report.restored) {
    if (!seen.has(filePath)) {
      seen.add(filePath);
      outcomes.push({ filePath, changeKind: "modified", kind: "restored" });
    }
  }
  result.report.skipped.forEach((issue) => addIssue("skipped", issue));
  result.report.failed.forEach((issue) => addIssue("failed", issue));
  return outcomes;
}

function DiffBody({ diff, label }: { diff: UndoDiffResult; label: string }) {
  return (
    <div className="file-body">
      <div className="diff-label">
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          aria-hidden="true"
        >
          <path d="M5 12h14M13 6l6 6-6 6" />
        </svg>
        {label}
      </div>
      <div className="diff-code">
        {diff.lines.map((line, index) => (
          <div key={index} className={`diff-line ${line.kind}`}>
            <span className="gutter">{line.oldLine ?? line.newLine ?? ""}</span>
            <span className="code-line">
              {line.kind === "add" ? "+ " : line.kind === "del" ? "- " : "  "}
              {line.text || " "}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function UndoFileRow({
  entry,
  diff,
  selected,
  open,
  onSelected,
  onToggle,
}: {
  entry: UndoEntry;
  diff: UndoDiffResult | null;
  selected: boolean;
  open: boolean;
  onSelected: (selected: boolean) => void;
  onToggle: () => void;
}) {
  const { t } = useI18n();
  const problem = previewProblem(entry);
  const kindLabel = t(`undoPanel.kind.${entry.change_kind}`);
  const detail = entry.already_undone
    ? t("undoPanel.file.alreadyUndone")
    : entry.stale
      ? t("undoPanel.file.stale")
      : problem === "binary"
        ? t("undoPanel.file.binary")
        : problem === "tooLarge"
          ? t("undoPanel.file.tooLarge", {
              size: formatBytes(entry.size_bytes),
            })
          : problem === "unsupported"
            ? t("undoPanel.file.unsupported")
            : entry.change_kind === "created"
              ? t("undoPanel.file.created")
              : entry.change_kind === "deleted"
                ? t("undoPanel.file.deleted")
                : t("undoPanel.file.modified");
  const diffLabel =
    entry.change_kind === "created"
      ? t("undoPanel.diff.created")
      : entry.change_kind === "deleted"
        ? t("undoPanel.diff.deleted")
        : t("undoPanel.diff.modified");

  return (
    <article
      className={`file${open ? " open" : ""}${problem ? " unpreviewable" : ""}${entry.already_undone ? " already-undone" : ""}${entry.stale ? " stale" : ""}`}
      data-change-kind={entry.change_kind}
    >
      <div className="file-head">
        <UndoCheckbox
          checked={selected}
          disabled={entry.already_undone || entry.stale}
          label={t("undoPanel.selectFile", { path: entry.file_path })}
          onChange={onSelected}
        />
        <button
          className="file-toggle file-toggle--interactive"
          type="button"
          disabled={!diff}
          aria-expanded={open}
          onClick={onToggle}
        >
          <svg className="chev" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M9 6l6 6-6 6" />
          </svg>
          <span className="file-name-wrap">
            <span className="file-name" title={entry.file_path}>
              {entry.file_path}
            </span>
            <span
              className={`file-kind${entry.change_kind === "created" ? " danger" : entry.change_kind === "deleted" ? " restore" : problem ? " warn" : ""}`}
            >
              {detail}
            </span>
          </span>
          <span className="stats">
            {diff && (
              <>
                <span className="add">+{diff.insertions}</span>
                <span className="del">−{diff.deletions}</span>
              </>
            )}
            <span
              className={`kind-badge ${changeKindClass(entry.change_kind)}`}
            >
              {problem === "binary"
                ? t("undoPanel.badge.binary")
                : problem
                  ? formatBytes(entry.size_bytes)
                  : kindLabel}
            </span>
          </span>
        </button>
      </div>
      {open && diff && <DiffBody diff={diff} label={diffLabel} />}
    </article>
  );
}

function ResultFileRow({
  outcome,
  diff,
  open,
  onToggle,
}: {
  outcome: Outcome;
  diff: UndoDiffResult | null;
  open: boolean;
  onToggle: () => void;
}) {
  const { t } = useI18n();
  const reasonKind =
    outcome.kind === "skipped" ? skipReasonKind(outcome.reason) : null;
  const expandable =
    (reasonKind === "changed" || outcome.kind === "restored") && diff !== null;
  const description =
    outcome.kind === "skipped"
      ? reasonKind === "changed"
        ? t("undoPanel.result.file.skippedChanged")
        : reasonKind === "unsafe"
          ? t("undoPanel.result.file.skippedUnsafe")
          : reasonKind === "alreadyUndone"
            ? t("undoPanel.result.file.skippedAlreadyUndone")
            : reasonKind === "stale"
              ? t("undoPanel.result.file.skippedStale")
              : t("undoPanel.result.file.skippedUnknown", {
                  reason: outcome.reason ?? "",
                })
      : outcome.kind === "failed"
        ? t("undoPanel.result.file.failed", { reason: outcome.reason ?? "" })
        : outcome.changeKind === "created"
          ? t("undoPanel.result.file.createdRestored")
          : outcome.changeKind === "deleted"
            ? t("undoPanel.result.file.deletedRestored")
            : t("undoPanel.result.file.restored");
  const badge =
    outcome.kind === "skipped"
      ? t("undoPanel.result.badge.skipped")
      : outcome.kind === "failed"
        ? t("undoPanel.result.badge.failed")
        : outcome.changeKind === "created"
          ? t("undoPanel.result.badge.deleted")
          : t("undoPanel.result.badge.restored");
  const rowContent = (
    <>
      {expandable ? (
        <svg className="chev" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 6l6 6-6 6" />
        </svg>
      ) : (
        <span className="chev-placeholder" aria-hidden="true" />
      )}
      <span className="file-name-wrap">
        <span className="file-name" title={outcome.filePath}>
          {outcome.filePath}
        </span>
        <span
          className={`file-kind${outcome.kind === "skipped" ? " warn" : outcome.kind === "failed" ? " danger" : ""}`}
        >
          {description}
        </span>
      </span>
      <span className="stats">
        <span className={`kind-badge ${outcome.kind}`}>{badge}</span>
      </span>
    </>
  );

  return (
    <article
      className={`file result-file${outcome.kind === "skipped" ? " skipped-file" : ""}${outcome.kind === "failed" ? " failed-file" : ""}${open ? " open" : ""}`}
      data-result={outcome.kind}
    >
      <div className="file-head">
        {expandable ? (
          <button
            className="file-toggle file-toggle--interactive"
            type="button"
            aria-expanded={open}
            onClick={onToggle}
          >
            {rowContent}
          </button>
        ) : (
          <div className="file-toggle">{rowContent}</div>
        )}
      </div>
      {open && diff && (
        <DiffBody diff={diff} label={t("undoPanel.result.changedDiff")} />
      )}
    </article>
  );
}

function BoundaryNotice() {
  const { t } = useI18n();
  return (
    <div className="boundary" data-testid="undo-boundary-notice">
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.9"
        aria-hidden="true"
      >
        <path d="M12 3l8 4v5c0 4.5-3.3 7.5-8 9-4.7-1.5-8-4.5-8-9V7z" />
        <path d="M12 8v5M12 16h.01" />
      </svg>
      <p>
        <strong>{t("undoPanel.boundary.title")}</strong>
        <br />
        {t("undoPanel.boundary.terminalPrefix")}
        <code>{t("undoPanel.boundary.rm")}</code>
        {t("undoPanel.boundary.separator")}
        <code>{t("undoPanel.boundary.sed")}</code>
        {t("undoPanel.boundary.terminalSuffix")}
      </p>
    </div>
  );
}

export function UndoReviewPanel({
  sessionId,
  runId,
  initialResult = null,
  onBack,
  onComplete,
}: Props) {
  const { t } = useI18n();
  const [entries, setEntries] = useState<UndoEntry[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [result, setResult] = useState<UndoResultRecord | null>(initialResult);
  const mountedRef = useRef(false);
  const targetRef = useRef({ sessionId, runId });
  if (
    targetRef.current.sessionId !== sessionId ||
    targetRef.current.runId !== runId
  ) {
    targetRef.current = { sessionId, runId };
  }

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    const target = targetRef.current;
    let cancelled = false;
    setEntries([]);
    setSelected(new Set());
    setOpen(new Set());
    setLoading(true);
    setLoadError(null);
    setActionError(null);
    setSubmitting(false);
    setResult(initialResult);
    void invoke<UndoEntry[]>("list_run_undo_entries", {
      sessionId,
      runId,
    })
      .then((nextEntries) => {
        if (cancelled || targetRef.current !== target) return;
        const safeEntries = nextEntries ?? [];
        setEntries(safeEntries);
        if (!initialResult) {
          setSelected(
            new Set(
              safeEntries
                .filter(
                  (entry) =>
                    !entry.already_undone &&
                    !entry.stale &&
                    entry.change_kind !== "deleted" &&
                    previewProblem(entry) === null,
                )
                .map((entry) => entry.file_path),
            ),
          );
        }
      })
      .catch((error) => {
        if (!cancelled && targetRef.current === target) {
          setLoadError(String(error));
        }
      })
      .finally(() => {
        if (!cancelled && targetRef.current === target) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // A new target remounts this state. initialResult is the persisted snapshot
    // for that target and intentionally does not restart loading after submit.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, runId]);

  const diffs = useMemo(
    () =>
      new Map(entries.map((entry) => [entry.file_path, buildUndoDiff(entry)])),
    [entries],
  );

  const toggleOpen = (filePath: string) => {
    setOpen((previous) => {
      const next = new Set(previous);
      if (next.has(filePath)) next.delete(filePath);
      else next.add(filePath);
      return next;
    });
  };

  const submitUndo = async () => {
    const request = buildUndoRequest(entries, selected);
    if (request.paths.length === 0 || submitting) return;
    const target = targetRef.current;
    const isCurrentTarget = () =>
      mountedRef.current && targetRef.current === target;
    setSubmitting(true);
    setActionError(null);
    try {
      const report = await invoke<UndoReport>("undo_run_edits", {
        sessionId: target.sessionId,
        runId: target.runId,
        paths: request.paths,
        expectedDigests: request.expectedDigests,
      });
      if (!isCurrentTarget()) return;
      const requested = new Set(request.paths);
      const record: UndoResultRecord = {
        session_id: target.sessionId,
        run_id: target.runId,
        report,
        selected_entries: entries
          .filter((entry) => requested.has(entry.file_path))
          .map(({ file_path, change_kind }) => ({ file_path, change_kind })),
        total_entries: entries.length,
      };
      setResult(record);
      setOpen(new Set());
      onComplete(record);
      try {
        const refreshed = await invoke<UndoEntry[]>("list_run_undo_entries", {
          sessionId: target.sessionId,
          runId: target.runId,
        });
        if (isCurrentTarget()) setEntries(refreshed ?? []);
      } catch {
        // The durable report remains renderable even if refreshing previews fails.
      }
    } catch (error) {
      if (isCurrentTarget()) setActionError(String(error));
    } finally {
      if (isCurrentTarget()) setSubmitting(false);
    }
  };

  const resultCount = result?.selected_entries.length ?? entries.length;
  const outcomes = result ? resultOutcomes(result) : [];

  return (
    <div
      className="review undo-review"
      aria-label={
        result ? t("undoPanel.result.aria") : t("undoPanel.checklist.aria")
      }
    >
      <div className="review-head">
        <div className="review-heading">
          <button
            className="back"
            type="button"
            aria-label={t("undoPanel.back")}
            title={t("undoPanel.back")}
            onClick={onBack}
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              aria-hidden="true"
            >
              <path d="M15 5l-7 7 7 7" />
            </svg>
          </button>
          <span className="review-title">
            {result
              ? t("undoPanel.result.title", { count: resultCount })
              : t("undoPanel.checklist.title", { count: entries.length })}
          </span>
          <span className="mode-pill">
            {result
              ? t("undoPanel.result.mode")
              : t("undoPanel.checklist.mode")}
          </span>
        </div>
        <div className="review-sub">
          {result
            ? t("undoPanel.result.subtitle")
            : t("undoPanel.diff.modified")}
        </div>
      </div>

      {result && (
        <div className="result-banner" role="status">
          <div className="result-main">
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.2"
              aria-hidden="true"
            >
              <path d="M5 12l4 4L19 6" />
            </svg>
            {t("undoPanel.result.restored", {
              count: result.report.restored.length,
            })}
          </div>
          {result.report.skipped.length > 0 && (
            <div className="result-skip">
              <b>
                {t("undoPanel.result.skipped", {
                  count: result.report.skipped.length,
                })}
              </b>
              <br />
              {t("undoPanel.result.skippedDetail")}
            </div>
          )}
          {result.report.failed.length > 0 && (
            <div className="result-failed">
              <b>
                {t("undoPanel.result.failed", {
                  count: result.report.failed.length,
                })}
              </b>
              {result.report.failed.map((failure) => (
                <div key={failure.file_path}>
                  {failure.file_path}: {failure.reason}
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      <div className="file-list">
        {loading && (
          <div className="undo-panel-state">{t("undoPanel.loading")}</div>
        )}
        {loadError && !result && (
          <div className="undo-panel-state undo-panel-state--error">
            {t("undoPanel.loadFailed", { reason: loadError })}
          </div>
        )}
        {!loading && !loadError && !result && entries.length === 0 && (
          <div className="undo-panel-state">{t("undoPanel.empty")}</div>
        )}
        {!loading &&
          !loadError &&
          !result &&
          entries.length > 0 &&
          entries.every((entry) => entry.stale) && (
            // P2（reviewer 建议·比「按钮直接消失」更好）：这一轮记录不是空的——按钮显示、
            // 点进来才发现每一行都禁选、确认按钮永远灰着，不解释的话像是功能坏了。这里给
            // 一句空态解释，而不是让 RunCard 直接不显示撤销入口（那样用户根本进不来，
            // 也就看不到「为什么」）。
            <div className="undo-panel-state">{t("undoPanel.allStale")}</div>
          )}
        {!result &&
          entries.map((entry) => (
            <UndoFileRow
              key={entry.file_path}
              entry={entry}
              diff={diffs.get(entry.file_path) ?? null}
              selected={selected.has(entry.file_path)}
              open={open.has(entry.file_path)}
              onSelected={(checked) =>
                setSelected((previous) => {
                  const next = new Set(previous);
                  if (checked) next.add(entry.file_path);
                  else next.delete(entry.file_path);
                  return next;
                })
              }
              onToggle={() => toggleOpen(entry.file_path)}
            />
          ))}
        {result &&
          outcomes.map((outcome) => {
            const entry = entries.find(
              (candidate) => candidate.file_path === outcome.filePath,
            );
            return (
              <ResultFileRow
                key={`${outcome.kind}:${outcome.filePath}`}
                outcome={outcome}
                diff={entry ? (diffs.get(entry.file_path) ?? null) : null}
                open={open.has(outcome.filePath)}
                onToggle={() => toggleOpen(outcome.filePath)}
              />
            );
          })}
      </div>

      <div className="review-foot">
        {actionError && (
          <div className="undo-action-error" role="alert">
            {t("undoPanel.undoFailed", { reason: actionError })}
          </div>
        )}
        <BoundaryNotice />
        {!result && (
          <button
            className="undo-primary"
            type="button"
            disabled={selected.size === 0 || submitting || loading}
            onClick={() => void submitUndo()}
          >
            {submitting
              ? t("undoPanel.undoing")
              : t("undoPanel.undoSelected", { count: selected.size })}
          </button>
        )}
      </div>
    </div>
  );
}
