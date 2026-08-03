import { useEffect, useState } from "react";
import { isMainstreamDiffFile, type FileDiff } from "../lib/parseDiff";
import { useI18n } from "../i18n";

type Props = {
  file: FileDiff & { undoable?: boolean };
  open: boolean;
  onToggle: () => void;
};

const LINE_CLASS: Record<string, string> = {
  add: "filediff__line filediff__line--add",
  del: "filediff__line filediff__line--del",
  hunk: "filediff__line filediff__line--hunk",
  ctx: "filediff__line",
};

export const LARGE_DIFF_LINE_THRESHOLD = 500;

// 对齐原型 review-simple：文件头 = 行首 chevron(SVG·展开 rotate) + 路径 + +N−N。
// 无 ADD/UPDATE/DELETE 文字动词（opus BLOCK-1）。status 仅作内部/aria 语义。
export function FileDiffCard({ file, open, onToggle }: Props) {
  const { t } = useI18n();
  const mainstream = isMainstreamDiffFile(file.path) && !file.binary;
  const [visibleLines, setVisibleLines] = useState(LARGE_DIFF_LINE_THRESHOLD);
  useEffect(() => setVisibleLines(LARGE_DIFF_LINE_THRESHOLD), [file.path]);
  const renderedLines = file.lines.slice(0, visibleLines);
  const hasMore = renderedLines.length < file.lines.length;

  if (!mainstream) {
    return (
      <div
        className="filediff filediff--placeholder"
        data-status={file.status}
        style={{ flexShrink: 0 }}
      >
        <div className="filediff__placeholder-head">
          <span className="filediff__path">{file.path}</span>
          {file.undoable === false && (
            <span className="filediff__source filediff__source--no-undo-record">
              <svg
                viewBox="0 0 16 16"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                aria-hidden
              >
                <path d="M6.25 3.75h-2.5v2.5" />
                <path d="M3.75 6.25a4.75 4.75 0 1 1 1.8 4.53" />
                <path d="M3.25 12.75l9.5-9.5" />
              </svg>
              {t("reviewPanel.noUndoRecord")}
            </span>
          )}
          <span className="filediff__stat">
            {file.insertions > 0 && (
              <span className="filediff__add">+{file.insertions}</span>
            )}
            {file.deletions > 0 && (
              <span className="filediff__del">−{file.deletions}</span>
            )}
          </span>
        </div>
        <div className="filediff__placeholder-row">
          <span className="filediff__placeholder">
            {t("reviewPanel.dataFileNotShown")}
          </span>
        </div>
      </div>
    );
  }

  return (
    <div
      className={`filediff${open ? " is-open" : ""}`}
      data-status={file.status}
      style={{ flexShrink: 0 }}
    >
      <button
        type="button"
        className="filediff__head"
        aria-expanded={open}
        aria-label={`${file.path}（${file.status}）`}
        onClick={onToggle}
      >
        <span className="filediff__chev" aria-hidden>
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.4"
          >
            <path d="M9 6l6 6-6 6" />
          </svg>
        </span>
        <span className="filediff__path">{file.path}</span>
        {file.undoable === false && (
          <span className="filediff__source filediff__source--no-undo-record">
            <svg
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              aria-hidden
            >
              <path d="M6.25 3.75h-2.5v2.5" />
              <path d="M3.75 6.25a4.75 4.75 0 1 1 1.8 4.53" />
              <path d="M3.25 12.75l9.5-9.5" />
            </svg>
            {t("reviewPanel.noUndoRecord")}
          </span>
        )}
        <span className="filediff__stat">
          {file.insertions > 0 && (
            <span className="filediff__add">+{file.insertions}</span>
          )}
          {file.deletions > 0 && (
            <span className="filediff__del">−{file.deletions}</span>
          )}
        </span>
      </button>
      {open && (
        <pre className="filediff__body">
          {renderedLines.map((l, i) => (
            <div key={i} className={LINE_CLASS[l.kind]}>
              {l.text || " "}
            </div>
          ))}
          {hasMore && (
            <button
              type="button"
              className="filediff__more"
              onClick={() =>
                setVisibleLines((count) => count + LARGE_DIFF_LINE_THRESHOLD)
              }
            >
              {t("reviewPanel.showMore")}
            </button>
          )}
        </pre>
      )}
    </div>
  );
}
