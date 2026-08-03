import type { FileDiff } from "../lib/parseDiff";
import type { ChangedFile } from "../types/agent";
import { useI18n } from "../i18n";

type CodeLine = FileDiff["lines"][number] & { kind: "add" | "del" };

function verbOf(file: FileDiff | undefined): string {
  if (file?.status === "added") return "Add";
  if (file?.status === "deleted") return "Delete";
  return "Update";
}

function isCodeLine(line: FileDiff["lines"][number]): line is CodeLine {
  return line.kind === "add" || line.kind === "del";
}

export function InlineDiffCard({
  file,
  changed,
  open,
  onToggle,
  onOpenReview,
}: {
  file?: FileDiff;
  changed: ChangedFile;
  open: boolean;
  onToggle: () => void;
  onOpenReview?: () => void;
}) {
  const { t } = useI18n();
  const codeLines = (file?.lines ?? []).filter(isCodeLine);
  return (
    <div className={`diffcard${open ? " is-open" : ""}`}>
      <button
        type="button"
        className="diff-head"
        aria-expanded={open}
        onClick={onToggle}
      >
        <span className="diff-verb">{verbOf(file)}</span>
        <span className="diff-file">{changed.path}</span>
        <span className="diff-stat">
          {changed.insertions > 0 && (
            <span className="diff-add">+{changed.insertions}</span>
          )}
          {changed.deletions > 0 && (
            <span className="diff-del">−{changed.deletions}</span>
          )}
        </span>
        <span className="diff-chev" aria-hidden>
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.4"
          >
            <path d="M9 6l6 6-6 6" />
          </svg>
        </span>
      </button>
      {open && (
        <div className="diff-body">
          {codeLines.length > 0 && (
            <pre className="diff-code">
              {codeLines.map((l, i) => (
                <div key={i} className={`diff-line diff-line--${l.kind}`}>
                  {l.text || " "}
                </div>
              ))}
            </pre>
          )}
          <div className="diff-foot">
            <button type="button" className="diff-open" onClick={onOpenReview}>
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                aria-hidden
              >
                <path d="M12 3v18M5 8l-3 3 3 3M19 8l3 3-3 3" />
              </svg>
              {t("inlineDiffCard.openInReview")}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
