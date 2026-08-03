import { useEffect, useMemo, useState } from "react";
import type { ReviewResult } from "../types/agent";
import { isMainstreamDiffFile, parseUnifiedDiff } from "../lib/parseDiff";
import { useI18n } from "../i18n";
import { FileDiffCard, LARGE_DIFF_LINE_THRESHOLD } from "./FileDiffCard";

type Props = {
  review: ReviewResult;
  /** 关闭 Review（回 picker / tab=null）。可选——RightPanel 在 Task 4 才接线·
   *  设可选避免 Task 3 中间态 RightPanel 调用缺必填 prop 致 tsc 挂（codex BLOCK）。 */
  onClose?: () => void;
};

export function ReviewPanel({ review, onClose }: Props) {
  const { t } = useI18n();
  const files = useMemo(() => {
    const capabilities = new Map(
      (review.files ?? []).map((file) => [file.path, file.undoable]),
    );
    return parseUnifiedDiff(review.patch).map((file, index) => ({
      ...file,
      undoable:
        capabilities.get(file.path) ?? review.files?.[index]?.undoable ?? false,
    }));
  }, [review.patch, review.files]);
  const defaultOpen = () =>
    new Set(
      files[0] &&
        isMainstreamDiffFile(files[0].path) &&
        !files[0].binary &&
        files[0].lines.length <= LARGE_DIFF_LINE_THRESHOLD
        ? [files[0].path]
        : [],
    );
  // 小型主流文本仍默认展开第一个；大型 diff 与不可预览文件默认保持紧凑。
  const [open, setOpen] = useState<Set<string>>(defaultOpen);
  const fileIdentity = files
    .map(
      (file) =>
        `${file.path}\u0000${file.status}\u0000${file.binary === true}\u0000${file.lines
          .map((line) => `${line.kind}:${line.text}`)
          .join("\u0002")}`,
    )
    .sort()
    .join("\u0001");
  useEffect(() => {
    setOpen(defaultOpen());
    // 顺序变化不重置；文件身份或正文规模变化才应用新 review 的默认态。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fileIdentity]);
  if (!review.has_changes) return null;
  const toggle = (path: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      next.has(path) ? next.delete(path) : next.add(path);
      return next;
    });
  // 状态摘要：回答用户真正在问的「我现在看的是哪部分 diff」——已提交（本会话各段自己
  // 提交的内容）与未提交（当前工作区相对 HEAD 的改动）分开说，且必须永远说真话：
  // 不知道就不说（两者都缺时不渲染这一行），不editorialize「改动保留在工作目录」这种
  // 在改动已经提交进 git 之后就变成假话的旧措辞。
  const committed = review.committed_files_changed ?? 0;
  const uncommitted = review.uncommitted_files_changed ?? 0;
  const statusText =
    committed > 0 && uncommitted > 0
      ? t("reviewPanel.statusCommittedAndUncommitted", {
          committed,
          uncommitted,
        })
      : committed > 0
        ? t("reviewPanel.statusCommittedOnly", { committed })
        : uncommitted > 0
          ? t("reviewPanel.statusUncommittedOnly", { uncommitted })
          : null;

  return (
    <div className="review">
      <div className="review__head">
        <span className="review__title">
          {t("reviewPanel.title", { count: files.length })}
        </span>
        <div className="review__actions">
          <button
            type="button"
            className="review__close"
            aria-label={t("reviewPanel.close")}
            title={t("reviewPanel.close")}
            onClick={onClose}
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              aria-hidden
            >
              <path d="M18 6L6 18M6 6l12 12" />
            </svg>
          </button>
        </div>
      </div>
      <div className="review__files">
        {files.map((f) => (
          <FileDiffCard
            key={f.path}
            file={f}
            open={open.has(f.path)}
            onToggle={() => toggle(f.path)}
          />
        ))}
      </div>
      {statusText && (
        <div className="review__foot">
          <span className="review__foot-dot" aria-hidden />
          <span className="review__foot-copy">
            <span>{statusText}</span>
          </span>
        </div>
      )}
    </div>
  );
}
